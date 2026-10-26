//! The rest of the state Blink keeps beside the command buffer and checks before GL sees a call
//! (the Chrome oracle ratchet): framebuffer attachments and the formats behind
//! them, the copy / compressed / 3D texture entry points, `blitFramebuffer`, `invalidate*Framebuffer`,
//! `clearBuffer*`, `readBuffer`, transform feedback, integer vertex attributes, a program's uniform
//! blocks and transform-feedback varyings, the `uniform*` checks and `drawingBufferStorage`.
//!
//! Each body is `WebGLRenderingContextBase` / `WebGL2RenderingContextBase`'s (Chromium 150) plus the
//! checks ANGLE makes behind it, measured against stock Chrome 150 (`core/korvus-runtime/tests/
//! fixtures/webgl2/`). Before this, `getFramebufferAttachmentParameter` answered `null` for every
//! attachment, `copyTexImage2D` was a stub that raised nothing for a bad level, `beginTransformFeedback`
//! with no program in use raised nothing, and `uniform3ui` on a `uvec4` stored the value.

use deno_core::v8;
use spec::{GlOp, GlQuery, GlQueryResult, GlUniformValue, Rgba8};
use std::collections::HashMap;

use super::{ask, device_number, edit, null_val, obj_id, object_of, push, set_result};
use crate::binding::protos::throw_type_error;

const INVALID_ENUM: u32 = 0x0500;
const INVALID_VALUE: u32 = 0x0501;
const INVALID_OPERATION: u32 = 0x0502;

const TEXTURE_2D: u32 = 0x0DE1;
const TEXTURE_CUBE_MAP: u32 = 0x8513;
const TEXTURE_3D: u32 = 0x806F;
const TEXTURE_2D_ARRAY: u32 = 0x8C1A;
const FRAMEBUFFER: u32 = 0x8D40;
const READ_FRAMEBUFFER: u32 = 0x8CA8;
const DRAW_FRAMEBUFFER: u32 = 0x8CA9;
const COLOR_ATTACHMENT0: u32 = 0x8CE0;
const DEPTH_ATTACHMENT: u32 = 0x8D00;
const STENCIL_ATTACHMENT: u32 = 0x8D20;
const DEPTH_STENCIL_ATTACHMENT: u32 = 0x821A;
const BACK: u32 = 0x0405;
const COLOR: u32 = 0x1800;
const DEPTH: u32 = 0x1801;
const STENCIL: u32 = 0x1802;
const DEPTH_STENCIL: u32 = 0x84F9;
const UNSIGNED_NORMALIZED: u32 = 0x8C17;
const SIGNED_NORMALIZED: u32 = 0x8F9C;
const FLOAT: u32 = 0x1406;
const INT: u32 = 0x1404;
const UNSIGNED_INT: u32 = 0x1405;
const LINEAR: u32 = 0x2601;
const SRGB: u32 = 0x8C40;

/// One framebuffer attachment.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Attach {
    texture: bool,
    id: u32,
    level: i32,
    /// The cube face a `framebufferTexture2D` named, 0 for any other target.
    face: u32,
    layer: i32,
    layered: bool,
}

/// The state this file keeps per context.
#[derive(Default)]
pub(crate) struct DrawState {
    /// (texture, level) -> its internal format.
    tex_format: HashMap<(u32, i32), u32>,
    /// Renderbuffer -> its internal format.
    rb_format: HashMap<u32, u32>,
    /// (framebuffer, attachment point) -> what is attached.
    attach: HashMap<(u32, u32), Attach>,
    /// Transform feedback object (0 = the default one) -> (active, paused).
    tf_state: HashMap<u32, (bool, bool)>,
    /// Vertex attribute -> set up by `vertexAttribIPointer`.
    attrib_integer: HashMap<u32, bool>,
    /// Vertex attribute -> its current value, when an integer call set it (1 = int, 2 = uint).
    attrib_int: HashMap<u32, (u8, [f64; 4])>,
    /// `drawingBufferStorage`'s size, and the canvas size it was made at.
    pub(crate) drawing_buffer: Option<((u32, u32), (u32, u32))>,
}

// ── format facts ────────────────────────────────────────────────────────────────────────────────

/// `(r, g, b, a, depth, stencil bits, component type, color encoding)` of a sized internal format.
fn format_info(f: u32) -> Option<(i32, i32, i32, i32, i32, i32, u32, u32)> {
    let (un, sn, fl, si, ui) = (UNSIGNED_NORMALIZED, SIGNED_NORMALIZED, FLOAT, INT, UNSIGNED_INT);
    Some(match f {
        0x8229 => (8, 0, 0, 0, 0, 0, un, LINEAR),
        0x8F94 => (8, 0, 0, 0, 0, 0, sn, LINEAR),
        0x822D => (16, 0, 0, 0, 0, 0, fl, LINEAR),
        0x822E => (32, 0, 0, 0, 0, 0, fl, LINEAR),
        0x8232 => (8, 0, 0, 0, 0, 0, ui, LINEAR),
        0x8231 => (8, 0, 0, 0, 0, 0, si, LINEAR),
        0x8234 => (16, 0, 0, 0, 0, 0, ui, LINEAR),
        0x8233 => (16, 0, 0, 0, 0, 0, si, LINEAR),
        0x8236 => (32, 0, 0, 0, 0, 0, ui, LINEAR),
        0x8235 => (32, 0, 0, 0, 0, 0, si, LINEAR),
        0x822B => (8, 8, 0, 0, 0, 0, un, LINEAR),
        0x8F95 => (8, 8, 0, 0, 0, 0, sn, LINEAR),
        0x822F => (16, 16, 0, 0, 0, 0, fl, LINEAR),
        0x8230 => (32, 32, 0, 0, 0, 0, fl, LINEAR),
        0x8238 => (8, 8, 0, 0, 0, 0, ui, LINEAR),
        0x8237 => (8, 8, 0, 0, 0, 0, si, LINEAR),
        0x823A => (16, 16, 0, 0, 0, 0, ui, LINEAR),
        0x8239 => (16, 16, 0, 0, 0, 0, si, LINEAR),
        0x823C => (32, 32, 0, 0, 0, 0, ui, LINEAR),
        0x823B => (32, 32, 0, 0, 0, 0, si, LINEAR),
        0x8051 => (8, 8, 8, 0, 0, 0, un, LINEAR),
        0x8C41 => (8, 8, 8, 0, 0, 0, un, SRGB),
        0x8D62 => (5, 6, 5, 0, 0, 0, un, LINEAR),
        0x8F96 => (8, 8, 8, 0, 0, 0, sn, LINEAR),
        0x8C3A => (11, 11, 10, 0, 0, 0, fl, LINEAR),
        0x8C3D => (9, 9, 9, 0, 0, 0, fl, LINEAR),
        0x881B => (16, 16, 16, 0, 0, 0, fl, LINEAR),
        0x8815 => (32, 32, 32, 0, 0, 0, fl, LINEAR),
        0x8D7D => (8, 8, 8, 0, 0, 0, ui, LINEAR),
        0x8D8F => (8, 8, 8, 0, 0, 0, si, LINEAR),
        0x8D77 => (16, 16, 16, 0, 0, 0, ui, LINEAR),
        0x8D89 => (16, 16, 16, 0, 0, 0, si, LINEAR),
        0x8D71 => (32, 32, 32, 0, 0, 0, ui, LINEAR),
        0x8D83 => (32, 32, 32, 0, 0, 0, si, LINEAR),
        0x8058 => (8, 8, 8, 8, 0, 0, un, LINEAR),
        0x8C43 => (8, 8, 8, 8, 0, 0, un, SRGB),
        0x8F97 => (8, 8, 8, 8, 0, 0, sn, LINEAR),
        0x8057 => (5, 5, 5, 1, 0, 0, un, LINEAR),
        0x8056 => (4, 4, 4, 4, 0, 0, un, LINEAR),
        0x8059 => (10, 10, 10, 2, 0, 0, un, LINEAR),
        0x881A => (16, 16, 16, 16, 0, 0, fl, LINEAR),
        0x8814 => (32, 32, 32, 32, 0, 0, fl, LINEAR),
        0x8D7C => (8, 8, 8, 8, 0, 0, ui, LINEAR),
        0x8D8E => (8, 8, 8, 8, 0, 0, si, LINEAR),
        0x906F => (10, 10, 10, 2, 0, 0, ui, LINEAR),
        0x8D76 => (16, 16, 16, 16, 0, 0, ui, LINEAR),
        0x8D88 => (16, 16, 16, 16, 0, 0, si, LINEAR),
        0x8D82 => (32, 32, 32, 32, 0, 0, si, LINEAR),
        0x8D70 => (32, 32, 32, 32, 0, 0, ui, LINEAR),
        0x81A5 => (0, 0, 0, 0, 16, 0, un, LINEAR),
        0x81A6 => (0, 0, 0, 0, 24, 0, un, LINEAR),
        0x8CAC => (0, 0, 0, 0, 32, 0, fl, LINEAR),
        0x88F0 => (0, 0, 0, 0, 24, 8, un, LINEAR),
        0x8CAD => (0, 0, 0, 0, 32, 8, fl, LINEAR),
        0x8D48 => (0, 0, 0, 0, 0, 8, un, LINEAR),
        _ => return None,
    })
}

/// The sized format an unsized `(format, type)` pair stands for (WebGL 1's `texImage2D` and the
/// unsized renderbuffer formats), or `f` itself when it is sized already.
fn sized(f: u32, ty: u32) -> u32 {
    match (f, ty) {
        (0x1908, 0x1401) => 0x8058,                   // RGBA / UNSIGNED_BYTE
        (0x1907, 0x1401) => 0x8051,                   // RGB / UNSIGNED_BYTE
        (0x1908, 0x8033) => 0x8056,                   // RGBA / UNSIGNED_SHORT_4_4_4_4
        (0x1908, 0x8034) => 0x8057,                   // RGBA / UNSIGNED_SHORT_5_5_5_1
        (0x1907, 0x8363) => 0x8D62,                   // RGB / UNSIGNED_SHORT_5_6_5
        (0x1908, 0x1406) => 0x8814,                   // RGBA / FLOAT
        (0x1907, 0x1406) => 0x8815,                   // RGB / FLOAT
        (0x1908, 0x140B) | (0x1908, 0x8D61) => 0x881A, // RGBA / HALF_FLOAT(_OES)
        (0x1907, 0x140B) | (0x1907, 0x8D61) => 0x881B,
        (0x8C42, _) => 0x8C43,                        // SRGB_ALPHA_EXT
        (0x8C40, _) => 0x8C41,                        // SRGB_EXT
        (0x1902, 0x1403) => 0x81A5,                   // DEPTH_COMPONENT / UNSIGNED_SHORT
        (0x1902, 0x1405) => 0x81A6,                   // DEPTH_COMPONENT / UNSIGNED_INT
        (DEPTH_STENCIL, _) => 0x88F0,
        _ => f,
    }
}

// ── tracking, from the bodies in `webgl.rs` ─────────────────────────────────────────────────────

/// `texImage2D` / `copyTexImage2D` defined `level` of the texture bound to `target`.
pub(super) fn note_tex_level(st: &mut super::WebGLState, target: u32, level: i32, internal: u32, ty: u32) {
    let bind = if (0x8515..=0x851A).contains(&target) { TEXTURE_CUBE_MAP } else { target };
    let tex = st.params.texture_for(bind);
    if tex != 0 {
        st.draw.tex_format.insert((tex, level), sized(internal, ty));
    }
}

/// `texStorage*` gave the texture bound to `target` its levels' format.
pub(super) fn note_storage(st: &mut super::WebGLState, target: u32, levels: i32, internal: u32) {
    let tex = st.params.texture_for(target);
    if tex != 0 {
        for l in 0..levels {
            st.draw.tex_format.insert((tex, l), internal);
        }
    }
}

/// `renderbufferStorage*` on the bound renderbuffer.
pub(super) fn note_renderbuffer(st: &mut super::WebGLState, internal: u32) {
    let rb = st.params.binding(0x8CA7);
    if rb != 0 {
        st.draw.rb_format.insert(rb, sized(internal, 0));
    }
}

fn fb_for(st: &super::WebGLState, target: u32) -> u32 {
    match target {
        READ_FRAMEBUFFER => st.params.binding(0x8CAA),
        _ => st.params.binding(0x8CA6),
    }
}

/// `framebufferTexture2D` / `framebufferRenderbuffer` / `framebufferTextureLayer` attached `a` (or,
/// with id 0, detached) at `attachment` of the framebuffer bound to `target`.
pub(super) fn note_attach(st: &mut super::WebGLState, target: u32, attachment: u32, a: Attach) {
    let fb = fb_for(st, target);
    if fb == 0 {
        return;
    }
    let points: &[u32] = if attachment == DEPTH_STENCIL_ATTACHMENT { &[DEPTH_ATTACHMENT, STENCIL_ATTACHMENT] } else { std::slice::from_ref(&attachment) };
    for p in points {
        if a.id == 0 {
            st.draw.attach.remove(&(fb, *p));
        } else {
            st.draw.attach.insert((fb, *p), a);
        }
    }
}

pub(super) fn texture_attach(id: u32, level: i32, textarget: u32) -> Attach {
    let face = if (0x8515..=0x851A).contains(&textarget) { textarget } else { 0 };
    Attach { texture: true, id, level, face, layer: 0, layered: false }
}
pub(super) fn renderbuffer_attach(id: u32) -> Attach {
    Attach { texture: false, id, level: 0, face: 0, layer: 0, layered: false }
}

/// An object went away: what was attached of it is detached (GL detaches a deleted object from the
/// bound framebuffer), and a deleted framebuffer takes its attachments with it.
pub(super) fn note_deleted(st: &mut super::WebGLState, id: u32) {
    st.draw.attach.retain(|(fb, _), a| *fb != id && a.id != id);
}

/// The internal format of what `a` attaches.
fn attach_format(st: &super::WebGLState, a: &Attach) -> Option<u32> {
    if a.texture {
        st.draw.tex_format.get(&(a.id, a.level)).copied()
    } else {
        st.draw.rb_format.get(&a.id).copied()
    }
}

/// `vertexAttribPointer` set attribute `index` up as a float attribute.
pub(super) fn note_float_pointer(st: &mut super::WebGLState, index: u32) {
    st.draw.attrib_integer.insert(index, false);
}
/// A `vertexAttrib*f` call set attribute `index`'s current value as floats.
pub(super) fn note_float_value(st: &mut super::WebGLState, index: u32) {
    st.draw.attrib_int.remove(&index);
}

/// `getParameter(TRANSFORM_FEEDBACK_ACTIVE | TRANSFORM_FEEDBACK_PAUSED)` for the bound object.
pub(super) fn tf_param(st: &super::WebGLState, pname: u32) -> Option<bool> {
    let tf = st.params.binding(0x8E25);
    let (active, paused) = st.draw.tf_state.get(&tf).copied().unwrap_or((false, false));
    match pname {
        0x8E24 => Some(active),
        0x8E23 => Some(paused),
        _ => None,
    }
}

/// The drawing buffer's size after `drawingBufferStorage`, while the canvas keeps the size it had.
pub(super) fn drawing_buffer(st: &super::WebGLState, canvas: (u32, u32)) -> Option<(u32, u32)> {
    match st.draw.drawing_buffer {
        Some((size, at)) if at == canvas => Some(size),
        _ => None,
    }
}

/// `checkFramebufferStatus` for a framebuffer with a LAYER attached (`framebufferTextureLayer`), which
/// the backend has no attachment for: complete when every attachment has a defined image of one
/// renderable format family, else INCOMPLETE_ATTACHMENT. `None` = not such a framebuffer.
pub(super) fn layered_status(st: &super::WebGLState, target: u32) -> Option<u32> {
    let fb = fb_for(st, target);
    if fb == 0 || !st.draw.attach.iter().any(|((f, _), a)| *f == fb && a.layered) {
        return None;
    }
    for ((f, _), a) in st.draw.attach.iter().filter(|((f, _), _)| *f == fb) {
        let _ = f;
        let defined = attach_format(st, a).is_some_and(|fmt| format_info(fmt).is_some());
        let sized_ok = if a.texture { st.gl2.levels.contains_key(&(a.id, a.level)) } else { true };
        if !defined || !sized_ok {
            return Some(0x8CD6); // FRAMEBUFFER_INCOMPLETE_ATTACHMENT
        }
        if a.layered {
            let depth = st.gl2.levels.get(&(a.id, a.level)).map(|l| l.2).unwrap_or(0);
            if a.layer as i64 >= depth {
                return Some(0x8CD6);
            }
        }
    }
    Some(0x8CD5) // FRAMEBUFFER_COMPLETE
}

// ── helpers ─────────────────────────────────────────────────────────────────────────────────────

fn synth(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, code: u32) {
    edit(scope, this, |s| s.synthesize(code));
}
fn u32_arg(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, i: i32) -> u32 {
    args.get(i).uint32_value(scope).unwrap_or(0)
}
fn i32_arg(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, i: i32) -> i32 {
    args.get(i).int32_value(scope).unwrap_or(0)
}
fn device(scope: &mut v8::PinScope, pname: u32, v2: bool, dflt: i64) -> i64 {
    device_number(scope, pname, v2).map(|n| n as i64).unwrap_or(dflt)
}
fn need(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, n: usize, iface: &str, member: &str) -> bool {
    crate::binding::protos::require_args(scope, args, n, iface, member)
}

/// A WebGLProgram argument, as Blink's `ValidateWebGLProgramOrShader`: the binding layer's TypeError
/// for anything else, INVALID_VALUE for another context's or a deleted one. `None` = refused.
fn program_arg(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, args: &v8::FunctionCallbackArguments, i: i32, member: &str, iface: &str) -> Option<u32> {
    let v = args.get(i);
    if !crate::binding::protos::implements(scope, v, "WebGLProgram") {
        throw_type_error(scope, &format!("Failed to execute '{member}' on '{iface}': parameter {} is not of type 'WebGLProgram'.", i + 1));
        return None;
    }
    let id = obj_id(scope, v);
    let mine = object_of(scope, this, id).is_some_and(|o| o.strict_equals(v));
    if !mine {
        synth(scope, this, INVALID_OPERATION);
        return None;
    }
    if edit(scope, this, |s| s.params.is_deleted(id)) {
        synth(scope, this, INVALID_VALUE);
        return None;
    }
    Some(id)
}

/// The texture bound for a 2D-family `target` (a cube face binds through TEXTURE_CUBE_MAP), with
/// Blink's `ValidateTexture2DBinding` errors: INVALID_ENUM for another target, INVALID_OPERATION when
/// nothing is bound. `None` = refused.
fn tex2d_binding(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, target: u32) -> Option<u32> {
    let bind = match target {
        TEXTURE_2D => TEXTURE_2D,
        0x8515..=0x851A => TEXTURE_CUBE_MAP,
        _ => {
            synth(scope, this, INVALID_ENUM);
            return None;
        }
    };
    let tex = edit(scope, this, |s| s.params.texture_for(bind));
    if tex == 0 {
        synth(scope, this, INVALID_OPERATION);
        return None;
    }
    Some(tex)
}
fn tex3d_binding(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, target: u32) -> Option<u32> {
    if !matches!(target, TEXTURE_3D | TEXTURE_2D_ARRAY) {
        synth(scope, this, INVALID_ENUM);
        return None;
    }
    let tex = edit(scope, this, |s| s.params.texture_for(target));
    if tex == 0 {
        synth(scope, this, INVALID_OPERATION);
        return None;
    }
    Some(tex)
}

/// Can `copyTexImage2D` make a texture of `internal` (WebGL 1: the five unsized formats; WebGL 2 the
/// sized ones a color buffer copies into too)?
fn copy_format_ok(internal: u32, v2: bool) -> bool {
    matches!(internal, 0x1906 | 0x1909 | 0x190A | 0x1907 | 0x1908)
        || (v2
            && matches!(
                internal,
                0x8229 | 0x822B | 0x8051 | 0x8058 | 0x8C41 | 0x8C43 | 0x8D62 | 0x8057 | 0x8056 | 0x8059 | 0x8232 | 0x8231
                    | 0x8234 | 0x8233 | 0x8236 | 0x8235 | 0x8238 | 0x8237 | 0x823A | 0x8239 | 0x823C | 0x823B | 0x8D7C
                    | 0x8D8E | 0x8D76 | 0x8D88 | 0x8D70 | 0x8D82 | 0x906F
            ))
}

/// The compressed formats this context accepts: the ones its enabled extensions add
/// (`COMPRESSED_TEXTURE_FORMATS`).
fn compressed_ok(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, format: u32) -> bool {
    edit(scope, this, |s| super::extensions::compressed_formats(s).contains(&format))
}

/// The bytes a pixels argument holds (an `ArrayBufferView`), or `None` for `null`.
fn view_len(v: v8::Local<v8::Value>) -> Option<usize> {
    v8::Local::<v8::ArrayBufferView>::try_from(v).ok().map(|a| a.byte_length())
}

/// Bytes per texel of `(format, type)` for the uploads WebGL 2 takes.
fn texel_bytes(format: u32, ty: u32) -> usize {
    let comps = match format {
        0x1903 | 0x8D94 | 0x1906 | 0x1909 | 0x1902 => 1, // RED, RED_INTEGER, ALPHA, LUMINANCE, DEPTH_COMPONENT
        0x8227 | 0x8228 | 0x190A | DEPTH_STENCIL => 2,  // RG, RG_INTEGER, LUMINANCE_ALPHA
        0x1907 | 0x8D98 => 3,                           // RGB, RGB_INTEGER
        _ => 4,
    };
    match ty {
        0x1400 | 0x1401 => comps,
        0x1402 | 0x1403 | 0x140B | 0x8D61 => comps * 2,
        0x8033 | 0x8034 | 0x8363 => 2,
        0x84FA | 0x8C3B | 0x8C3E | 0x8368 => 4, // UNSIGNED_INT_24_8, 10F_11F_11F_REV, 5_9_9_9_REV, 2_10_10_10_REV
        0x8DAD => 8,                            // FLOAT_32_UNSIGNED_INT_24_8_REV
        _ => comps * 4,
    }
}

// ── the calls ───────────────────────────────────────────────────────────────────────────────────

/// The calls this file answers. `false` = not one of them.
pub(super) fn call(
    scope: &mut v8::PinScope,
    this: v8::Local<v8::Object>,
    member: &str,
    args: &v8::FunctionCallbackArguments,
    rv: &mut v8::ReturnValue,
    iface: &str,
) -> bool {
    let v2 = iface == "WebGL2RenderingContext";
    match member {
        "getFramebufferAttachmentParameter" => {
            if !need(scope, args, 3, iface, member) {
                return true;
            }
            let (target, attachment, pname) = (u32_arg(scope, args, 0), u32_arg(scope, args, 1), u32_arg(scope, args, 2));
            framebuffer_attachment_parameter(scope, this, target, attachment, pname, v2, rv);
        }
        "copyTexImage2D" => {
            if !need(scope, args, 8, iface, member) {
                return true;
            }
            let target = u32_arg(scope, args, 0);
            let level = i32_arg(scope, args, 1);
            let internal = u32_arg(scope, args, 2);
            let (x, y, w, h, border) = (i32_arg(scope, args, 3), i32_arg(scope, args, 4), i32_arg(scope, args, 5), i32_arg(scope, args, 6), i32_arg(scope, args, 7));
            let Some(tex) = tex2d_binding(scope, this, target) else { return true };
            if !copy_format_ok(internal, v2) {
                synth(scope, this, INVALID_ENUM);
                return true;
            }
            let max = device(scope, 0x0D33, v2, 4096);
            if level < 0 || w < 0 || h < 0 || border != 0 || (w as i64) > max || (h as i64) > max || level > 31 {
                synth(scope, this, INVALID_VALUE);
                return true;
            }
            if edit(scope, this, |s| s.gl2.immutable.contains_key(&tex)) {
                synth(scope, this, INVALID_OPERATION);
                return true;
            }
            // The copy itself: the read framebuffer's pixels, as `readPixels` reads them.
            let pixels = match ask(scope, this, GlQuery::ReadPixels { x, y, width: w as u32, height: h as u32 }) {
                GlQueryResult::Bytes(b) => Some(Rgba8 { width: w as u32, height: h as u32, data: b }),
                _ => None,
            };
            edit(scope, this, |s| {
                s.gl2.levels.insert((tex, level), (w as i64, h as i64, 1));
                note_tex_level(s, target, level, internal, 0x1401);
            });
            push(scope, this, GlOp::TexImage2D { target, level, internal_format: internal, width: w as u32, height: h as u32, format: 0x1908, ty: 0x1401, pixels });
        }
        "copyTexSubImage2D" | "copyTexSubImage3D" => {
            let three = member == "copyTexSubImage3D";
            if !need(scope, args, if three { 9 } else { 8 }, iface, member) {
                return true;
            }
            let target = u32_arg(scope, args, 0);
            let level = i32_arg(scope, args, 1);
            let base = if three { 5 } else { 4 };
            let xoff = i32_arg(scope, args, 2) as i64;
            let yoff = i32_arg(scope, args, 3) as i64;
            let zoff = if three { i32_arg(scope, args, 4) as i64 } else { 0 };
            let (x, y, w, h) = (i32_arg(scope, args, base), i32_arg(scope, args, base + 1), i32_arg(scope, args, base + 2), i32_arg(scope, args, base + 3));
            let tex = if three { tex3d_binding(scope, this, target) } else { tex2d_binding(scope, this, target) };
            let Some(tex) = tex else { return true };
            if level < 0 || xoff < 0 || yoff < 0 || zoff < 0 || w < 0 || h < 0 {
                synth(scope, this, INVALID_VALUE);
                return true;
            }
            let dims = edit(scope, this, |s| s.gl2.levels.get(&(tex, level)).copied()).unwrap_or((0, 0, 0));
            if xoff + w as i64 > dims.0 || yoff + h as i64 > dims.1 || (three && zoff >= dims.2) {
                synth(scope, this, INVALID_VALUE);
                return true;
            }
            if !three {
                let pixels = match ask(scope, this, GlQuery::ReadPixels { x, y, width: w as u32, height: h as u32 }) {
                    GlQueryResult::Bytes(b) => Rgba8 { width: w as u32, height: h as u32, data: b },
                    _ => return true,
                };
                push(scope, this, GlOp::TexSubImage2D { target, level, xoffset: xoff as i32, yoffset: yoff as i32, format: 0x1908, ty: 0x1401, pixels });
            }
        }
        "compressedTexImage2D" | "compressedTexImage3D" | "compressedTexSubImage2D" | "compressedTexSubImage3D" => {
            let three = member.ends_with("3D");
            let sub = member.contains("Sub");
            let n = match (three, sub) {
                (false, false) => 7,
                (true, false) => 8,
                (false, true) => 8,
                (true, true) => 10,
            };
            if !need(scope, args, n, iface, member) {
                return true;
            }
            let target = u32_arg(scope, args, 0);
            let format_at = match (three, sub) {
                (false, false) => 2,
                (true, false) => 2,
                (false, true) => 6,
                (true, true) => 8,
            };
            let format = u32_arg(scope, args, format_at);
            let tex = if three { tex3d_binding(scope, this, target) } else { tex2d_binding(scope, this, target) };
            if tex.is_none() {
                return true;
            }
            // Blink's `ValidateCompressedTexFormat`: only a format an enabled extension adds.
            if !compressed_ok(scope, this, format) {
                synth(scope, this, INVALID_ENUM);
                return true;
            }
            let level = i32_arg(scope, args, 1);
            if level < 0 {
                synth(scope, this, INVALID_VALUE);
            }
        }
        "texImage3D" => {
            if !need(scope, args, 10, iface, member) {
                return true;
            }
            let target = u32_arg(scope, args, 0);
            let level = i32_arg(scope, args, 1);
            let internal = u32_arg(scope, args, 2);
            let (w, h, d, border) = (i32_arg(scope, args, 3), i32_arg(scope, args, 4), i32_arg(scope, args, 5), i32_arg(scope, args, 6));
            let (format, ty) = (u32_arg(scope, args, 7), u32_arg(scope, args, 8));
            let Some(tex) = tex3d_binding(scope, this, target) else { return true };
            if level < 0 || w < 0 || h < 0 || d < 0 || border != 0 {
                synth(scope, this, INVALID_VALUE);
                return true;
            }
            if let Some(len) = view_len(args.get(9)) {
                let need_bytes = texel_bytes(format, ty) * w as usize * h as usize * d as usize;
                if len < need_bytes {
                    synth(scope, this, INVALID_OPERATION);
                    return true;
                }
            }
            if edit(scope, this, |s| s.gl2.immutable.contains_key(&tex)) {
                synth(scope, this, INVALID_OPERATION);
                return true;
            }
            edit(scope, this, |s| {
                s.gl2.levels.insert((tex, level), (w as i64, h as i64, d as i64));
                s.draw.tex_format.insert((tex, level), sized(internal, ty));
            });
        }
        "texSubImage3D" => {
            if !need(scope, args, 11, iface, member) {
                return true;
            }
            let target = u32_arg(scope, args, 0);
            let level = i32_arg(scope, args, 1);
            let (x, y, z) = (i32_arg(scope, args, 2) as i64, i32_arg(scope, args, 3) as i64, i32_arg(scope, args, 4) as i64);
            let (w, h, d) = (i32_arg(scope, args, 5), i32_arg(scope, args, 6), i32_arg(scope, args, 7));
            let (format, ty) = (u32_arg(scope, args, 8), u32_arg(scope, args, 9));
            let Some(tex) = tex3d_binding(scope, this, target) else { return true };
            if level < 0 || x < 0 || y < 0 || z < 0 || w < 0 || h < 0 || d < 0 {
                synth(scope, this, INVALID_VALUE);
                return true;
            }
            if let Some(len) = view_len(args.get(10)) {
                let need_bytes = texel_bytes(format, ty) * w as usize * h as usize * d as usize;
                if len < need_bytes {
                    synth(scope, this, INVALID_OPERATION);
                    return true;
                }
            }
            let Some(dims) = edit(scope, this, |s| s.gl2.levels.get(&(tex, level)).copied()) else {
                // No image at that level: there is nothing to update.
                synth(scope, this, INVALID_OPERATION);
                return true;
            };
            if x + w as i64 > dims.0 || y + h as i64 > dims.1 || z + d as i64 > dims.2 {
                synth(scope, this, INVALID_VALUE);
            }
        }
        "framebufferTextureLayer" => {
            if !need(scope, args, 5, iface, member) {
                return true;
            }
            let target = u32_arg(scope, args, 0);
            let attachment = u32_arg(scope, args, 1);
            let (level, layer) = (i32_arg(scope, args, 3), i32_arg(scope, args, 4));
            if !matches!(target, FRAMEBUFFER | READ_FRAMEBUFFER | DRAW_FRAMEBUFFER) || !valid_fb_attachment(scope, attachment, v2) {
                synth(scope, this, INVALID_ENUM);
                return true;
            }
            let tv = args.get(2);
            let tex = if tv.is_null_or_undefined() {
                0
            } else {
                if !crate::binding::protos::implements(scope, tv, "WebGLTexture") {
                    throw_type_error(scope, &format!("Failed to execute '{member}' on '{iface}': parameter 3 is not of type 'WebGLTexture'."));
                    return true;
                }
                obj_id(scope, tv)
            };
            if edit(scope, this, |s| fb_for(s, target)) == 0 {
                synth(scope, this, INVALID_OPERATION);
                return true;
            }
            if tex != 0 {
                let tt = edit(scope, this, |s| s.params.texture_target(tex));
                if !matches!(tt, Some(TEXTURE_3D) | Some(TEXTURE_2D_ARRAY)) {
                    synth(scope, this, INVALID_OPERATION);
                    return true;
                }
                let max_layers = if tt == Some(TEXTURE_3D) { device(scope, 0x8073, true, 256) } else { device(scope, 0x88FF, true, 256) };
                if layer < 0 || layer as i64 >= max_layers || level < 0 {
                    synth(scope, this, INVALID_VALUE);
                    return true;
                }
            }
            let a = Attach { texture: true, id: tex, level, face: 0, layer, layered: true };
            edit(scope, this, |s| note_attach(s, target, attachment, a));
        }
        "blitFramebuffer" => {
            if !need(scope, args, 10, iface, member) {
                return true;
            }
            let mask = u32_arg(scope, args, 8);
            let filter = u32_arg(scope, args, 9);
            if mask & !(0x4000 | 0x0100 | 0x0400) != 0 {
                synth(scope, this, INVALID_VALUE);
                return true;
            }
            if !matches!(filter, 0x2600 | 0x2601) {
                synth(scope, this, INVALID_ENUM);
                return true;
            }
            if mask & (0x0100 | 0x0400) != 0 && filter != 0x2600 {
                synth(scope, this, INVALID_OPERATION);
                return true;
            }
            let (read, draw) = edit(scope, this, |s| (s.params.binding(0x8CAA), s.params.binding(0x8CA6)));
            if read == draw {
                // Reading and drawing one framebuffer (the default one included) is a feedback loop.
                synth(scope, this, INVALID_OPERATION);
            }
        }
        "invalidateFramebuffer" | "invalidateSubFramebuffer" => {
            let sub = member == "invalidateSubFramebuffer";
            if !need(scope, args, if sub { 6 } else { 2 }, iface, member) {
                return true;
            }
            let target = u32_arg(scope, args, 0);
            if !matches!(target, FRAMEBUFFER | READ_FRAMEBUFFER | DRAW_FRAMEBUFFER) {
                synth(scope, this, INVALID_ENUM);
                return true;
            }
            let list = super::i32_seq(scope, args.get(1));
            let fb = edit(scope, this, |s| fb_for(s, target));
            let max_color = device(scope, 0x8CDF, true, 8) as u32;
            for a in list {
                let a = a as u32;
                let ok = if fb == 0 {
                    matches!(a, COLOR | DEPTH | STENCIL)
                } else {
                    matches!(a, DEPTH_ATTACHMENT | STENCIL_ATTACHMENT | DEPTH_STENCIL_ATTACHMENT) || (COLOR_ATTACHMENT0..COLOR_ATTACHMENT0 + max_color).contains(&a)
                };
                if !ok {
                    synth(scope, this, INVALID_ENUM);
                    return true;
                }
            }
            if sub && (i32_arg(scope, args, 4) < 0 || i32_arg(scope, args, 5) < 0) {
                synth(scope, this, INVALID_VALUE);
            }
        }
        "readBuffer" => {
            if !need(scope, args, 1, iface, member) {
                return true;
            }
            let src = u32_arg(scope, args, 0);
            if !(src == BACK || src == 0 || (COLOR_ATTACHMENT0..=COLOR_ATTACHMENT0 + 15).contains(&src)) {
                synth(scope, this, INVALID_ENUM);
                return true;
            }
            let max_color = device(scope, 0x8CDF, true, 8) as u32;
            let e = edit(scope, this, |s| {
                let fb = s.params.binding(0x8CAA);
                if fb != 0 && src >= COLOR_ATTACHMENT0 + max_color {
                    return Some(INVALID_OPERATION);
                }
                s.params.read_buffer(src)
            });
            if let Some(e) = e {
                synth(scope, this, e);
            }
        }
        "clearBufferfv" | "clearBufferiv" | "clearBufferuiv" | "clearBufferfi" => {
            clear_buffer(scope, this, member, args, iface);
        }
        "bindTransformFeedback" => {
            if !need(scope, args, 2, iface, member) {
                return true;
            }
            let target = u32_arg(scope, args, 0);
            if target != 0x8E22 {
                synth(scope, this, INVALID_ENUM);
                return true;
            }
            let id = obj_id(scope, args.get(1));
            let busy = edit(scope, this, |s| {
                let cur = s.params.binding(0x8E25);
                let (a, p) = s.draw.tf_state.get(&cur).copied().unwrap_or((false, false));
                a && !p
            });
            if busy {
                synth(scope, this, INVALID_OPERATION);
                return true;
            }
            if id != 0 {
                edit(scope, this, |s| {
                    s.bound.insert(id);
                });
            }
            edit(scope, this, |s| s.params.bind_transform_feedback(id));
        }
        "beginTransformFeedback" => {
            if !need(scope, args, 1, iface, member) {
                return true;
            }
            let mode = u32_arg(scope, args, 0);
            if !matches!(mode, 0 | 1 | 4) {
                synth(scope, this, INVALID_ENUM);
                return true;
            }
            let (program, tf, active) = edit(scope, this, |s| {
                let tf = s.params.binding(0x8E25);
                (s.params.binding(0x8B8D), tf, s.draw.tf_state.get(&tf).map(|t| t.0).unwrap_or(false))
            });
            if active || program == 0 {
                synth(scope, this, INVALID_OPERATION);
                return true;
            }
            let count = match ask(scope, this, GlQuery::ProgramParameter { program, pname: 0x8C83 }) {
                GlQueryResult::Int(n) => n.max(0) as u32,
                _ => 0,
            };
            let separate = matches!(ask(scope, this, GlQuery::ProgramParameter { program, pname: 0x8C7F }), GlQueryResult::UInt(0x8C8D));
            if count == 0 {
                synth(scope, this, INVALID_OPERATION);
                return true;
            }
            let needed = if separate { count } else { 1 };
            let bound_all = edit(scope, this, |s| (0..needed).all(|i| s.gl2.tf_binding(i) != 0));
            if !bound_all {
                synth(scope, this, INVALID_OPERATION);
                return true;
            }
            edit(scope, this, |s| {
                s.draw.tf_state.insert(tf, (true, false));
            });
        }
        "endTransformFeedback" | "pauseTransformFeedback" | "resumeTransformFeedback" => {
            let (tf, (active, paused)) = edit(scope, this, |s| {
                let tf = s.params.binding(0x8E25);
                (tf, s.draw.tf_state.get(&tf).copied().unwrap_or((false, false)))
            });
            let next = match member {
                "endTransformFeedback" if active => Some((false, false)),
                "pauseTransformFeedback" if active && !paused => Some((true, true)),
                "resumeTransformFeedback" if active && paused => Some((true, false)),
                _ => None,
            };
            match next {
                Some(n) => edit(scope, this, |s| {
                    s.draw.tf_state.insert(tf, n);
                }),
                None => synth(scope, this, INVALID_OPERATION),
            }
        }
        "transformFeedbackVaryings" => {
            if !need(scope, args, 3, iface, member) {
                return true;
            }
            let Some(names) = string_seq_arg(scope, args.get(1), member, iface, 1) else { return true };
            let Some(program) = program_arg(scope, this, args, 0, member, iface) else { return true };
            let mode = u32_arg(scope, args, 2);
            if !matches!(mode, 0x8C8C | 0x8C8D) {
                synth(scope, this, INVALID_ENUM);
                return true;
            }
            if mode == 0x8C8D && names.len() as i64 > device(scope, 0x8C8B, true, 4) {
                synth(scope, this, INVALID_VALUE);
                return true;
            }
            push(scope, this, GlOp::TransformFeedbackVaryings { program, varyings: names, mode });
        }
        "getTransformFeedbackVarying" => {
            if !need(scope, args, 2, iface, member) {
                return true;
            }
            let Some(program) = program_arg(scope, this, args, 0, member, iface) else {
                rv.set(null_val(scope));
                return true;
            };
            let index = u32_arg(scope, args, 1);
            match ask(scope, this, GlQuery::TransformFeedbackVarying { program, index }) {
                GlQueryResult::ActiveInfo { name, size, ty } => {
                    let o = crate::install::live::mint_table(scope, "WebGLActiveInfo");
                    let nv = crate::binding::protos::str_val(scope, &name);
                    super::set_slot(scope, o, "name", nv);
                    let sv = v8::Integer::new(scope, size).into();
                    super::set_slot(scope, o, "size", sv);
                    let tv = v8::Number::new(scope, ty as f64).into();
                    super::set_slot(scope, o, "type", tv);
                    rv.set(o.into());
                }
                _ => {
                    synth(scope, this, INVALID_VALUE);
                    rv.set(null_val(scope));
                }
            }
        }
        "uniformBlockBinding" => {
            if !need(scope, args, 3, iface, member) {
                return true;
            }
            let Some(program) = program_arg(scope, this, args, 0, member, iface) else { return true };
            let (index, binding) = (u32_arg(scope, args, 1), u32_arg(scope, args, 2));
            if !block_index_ok(scope, this, program, index) || binding as i64 >= device(scope, 0x8A2F, true, 24) {
                synth(scope, this, INVALID_VALUE);
                return true;
            }
            push(scope, this, GlOp::UniformBlockBinding { program, index, binding });
        }
        "getUniformBlockIndex" => {
            if !need(scope, args, 2, iface, member) {
                return true;
            }
            let Some(program) = program_arg(scope, this, args, 0, member, iface) else {
                rv.set_uint32(u32::MAX);
                return true;
            };
            let name = args.get(1).to_rust_string_lossy(scope);
            match ask(scope, this, GlQuery::UniformBlockIndex { program, name }) {
                GlQueryResult::UInt(i) => rv.set_uint32(i),
                _ => rv.set_uint32(u32::MAX),
            }
        }
        "getActiveUniformBlockName" => {
            if !need(scope, args, 2, iface, member) {
                return true;
            }
            let Some(program) = program_arg(scope, this, args, 0, member, iface) else {
                rv.set(null_val(scope));
                return true;
            };
            let index = u32_arg(scope, args, 1);
            match ask(scope, this, GlQuery::UniformBlockName { program, index }) {
                GlQueryResult::Str(s) => rv.set(crate::binding::protos::str_val(scope, &s)),
                _ => {
                    synth(scope, this, INVALID_VALUE);
                    rv.set(null_val(scope));
                }
            }
        }
        "getActiveUniformBlockParameter" => {
            if !need(scope, args, 3, iface, member) {
                return true;
            }
            let Some(program) = program_arg(scope, this, args, 0, member, iface) else {
                rv.set(null_val(scope));
                return true;
            };
            let (index, pname) = (u32_arg(scope, args, 1), u32_arg(scope, args, 2));
            if !block_index_ok(scope, this, program, index) {
                synth(scope, this, INVALID_VALUE);
                rv.set(null_val(scope));
                return true;
            }
            if !matches!(pname, 0x8A3F | 0x8A40 | 0x8A42 | 0x8A43 | 0x8A44 | 0x8A46) {
                synth(scope, this, INVALID_ENUM);
                rv.set(null_val(scope));
                return true;
            }
            let r = ask(scope, this, GlQuery::UniformBlockParameter { program, index, pname });
            set_result(scope, rv, r);
        }
        "getUniformIndices" => {
            if !need(scope, args, 2, iface, member) {
                return true;
            }
            let Some(names) = string_seq_arg(scope, args.get(1), member, iface, 1) else { return true };
            let Some(program) = program_arg(scope, this, args, 0, member, iface) else {
                rv.set(null_val(scope));
                return true;
            };
            match ask(scope, this, GlQuery::UniformIndices { program, names }) {
                GlQueryResult::UInts(v) => {
                    let items: Vec<v8::Local<v8::Value>> = v.iter().map(|n| v8::Number::new(scope, *n as f64).into()).collect();
                    rv.set(v8::Array::new_with_elements(scope, &items).into());
                }
                _ => rv.set(null_val(scope)),
            }
        }
        "getActiveUniforms" => {
            if !need(scope, args, 3, iface, member) {
                return true;
            }
            let Some(program) = program_arg(scope, this, args, 0, member, iface) else {
                rv.set(null_val(scope));
                return true;
            };
            let indices: Vec<u32> = super::i32_seq(scope, args.get(1)).into_iter().map(|v| v as u32).collect();
            let pname = u32_arg(scope, args, 2);
            if !matches!(pname, 0x8A37 | 0x8A38 | 0x8A3A | 0x8A3B | 0x8A3C | 0x8A3D | 0x8A3E) {
                synth(scope, this, INVALID_ENUM);
                rv.set(null_val(scope));
                return true;
            }
            let count = match ask(scope, this, GlQuery::ProgramParameter { program, pname: 0x8B86 }) {
                GlQueryResult::Int(n) => n.max(0) as u32,
                _ => 0,
            };
            if indices.iter().any(|i| *i >= count) {
                synth(scope, this, INVALID_VALUE);
                rv.set(null_val(scope));
                return true;
            }
            let items: Vec<v8::Local<v8::Value>> = match ask(scope, this, GlQuery::ActiveUniforms { program, indices, pname }) {
                GlQueryResult::UInts(v) => v.iter().map(|n| v8::Number::new(scope, *n as f64).into()).collect(),
                GlQueryResult::Ints(v) => v.iter().map(|n| v8::Number::new(scope, *n as f64).into()).collect(),
                GlQueryResult::Bools(v) => v.iter().map(|b| v8::Boolean::new(scope, *b).into()).collect(),
                _ => {
                    rv.set(null_val(scope));
                    return true;
                }
            };
            rv.set(v8::Array::new_with_elements(scope, &items).into());
        }
        "getFragDataLocation" => {
            if !need(scope, args, 2, iface, member) {
                return true;
            }
            let Some(program) = program_arg(scope, this, args, 0, member, iface) else {
                rv.set_int32(-1);
                return true;
            };
            let name = args.get(1).to_rust_string_lossy(scope);
            if !matches!(ask(scope, this, GlQuery::ProgramParameter { program, pname: 0x8B82 }), GlQueryResult::Bool(true)) {
                synth(scope, this, INVALID_OPERATION);
                rv.set_int32(-1);
                return true;
            }
            match ask(scope, this, GlQuery::FragDataLocation { program, name }) {
                GlQueryResult::Int(i) => rv.set_int32(i),
                _ => rv.set_int32(-1),
            }
        }
        "vertexAttribIPointer" => {
            if !need(scope, args, 5, iface, member) {
                return true;
            }
            let (index, size, ty) = (u32_arg(scope, args, 0), i32_arg(scope, args, 1), u32_arg(scope, args, 2));
            let (stride, offset) = (i32_arg(scope, args, 3), args.get(4).number_value(scope).unwrap_or(0.0));
            if index as i64 >= device(scope, 0x8869, v2, 16) {
                synth(scope, this, INVALID_VALUE);
                return true;
            }
            let tsize = match ty {
                0x1400 | 0x1401 => 1,
                0x1402 | 0x1403 => 2,
                0x1404 | 0x1405 => 4,
                _ => {
                    synth(scope, this, INVALID_ENUM);
                    return true;
                }
            };
            if !(1..=4).contains(&size) || !(0..=255).contains(&stride) || offset < 0.0 {
                synth(scope, this, INVALID_VALUE);
                return true;
            }
            let off = offset as i64;
            if stride % tsize != 0 || off % tsize as i64 != 0 {
                synth(scope, this, INVALID_OPERATION);
                return true;
            }
            if edit(scope, this, |s| s.params.buffer_for(0x8892)) == 0 && off != 0 {
                synth(scope, this, INVALID_OPERATION);
                return true;
            }
            edit(scope, this, |s| {
                s.draw.attrib_integer.insert(index, true);
            });
            push(scope, this, GlOp::VertexAttribPointer { index, size: size as u32, ty, normalized: false, stride: stride as u32, offset: off as u32 });
        }
        "vertexAttribI4i" | "vertexAttribI4ui" | "vertexAttribI4iv" | "vertexAttribI4uiv" => {
            if !need(scope, args, 2, iface, member) {
                return true;
            }
            let index = u32_arg(scope, args, 0);
            let unsigned = member.contains("ui");
            let vals: Vec<f64> = if member.ends_with('v') {
                let v = args.get(1);
                let seq: Vec<f64> = match v8::Local::<v8::Object>::try_from(v) {
                    Ok(o) => {
                        let n = crate::binding::protos::get_prop(scope, o, "length").and_then(|l| l.uint32_value(scope)).unwrap_or(0);
                        (0..n).filter_map(|i| o.get_index(scope, i)).map(|x| x.number_value(scope).unwrap_or(0.0)).collect()
                    }
                    Err(_) => {
                        throw_type_error(scope, &format!("Failed to execute '{member}' on '{iface}': The provided value cannot be converted to a sequence."));
                        return true;
                    }
                };
                if seq.len() < 4 {
                    synth(scope, this, INVALID_VALUE);
                    return true;
                }
                seq
            } else {
                (1..5).map(|i| args.get(i).number_value(scope).unwrap_or(0.0)).collect()
            };
            if index as i64 >= device(scope, 0x8869, v2, 16) {
                synth(scope, this, INVALID_VALUE);
                return true;
            }
            let conv = |x: f64| -> f64 {
                let t = if x.is_finite() { x.trunc() } else { 0.0 };
                if unsigned { (t as i64).rem_euclid(1 << 32) as f64 } else { (t as i64).rem_euclid(1 << 32) as u32 as i32 as f64 }
            };
            let v4 = [conv(vals[0]), conv(vals[1]), conv(vals[2]), conv(vals[3])];
            edit(scope, this, |s| {
                s.draw.attrib_int.insert(index, (if unsigned { 2 } else { 1 }, v4));
            });
            push(scope, this, GlOp::VertexAttrib { index, values: [v4[0] as f32, v4[1] as f32, v4[2] as f32, v4[3] as f32] });
        }
        "drawingBufferStorage" => {
            if !need(scope, args, 3, iface, member) {
                return true;
            }
            let format = u32_arg(scope, args, 0);
            let (w, h) = (i32_arg(scope, args, 1), i32_arg(scope, args, 2));
            if !matches!(format, 0x8058 | 0x8C43 | 0x881A) {
                synth(scope, this, INVALID_ENUM);
                return true;
            }
            let max = device(scope, 0x84E8, v2, 16384);
            if w < 0 || h < 0 || w as i64 > max || h as i64 > max {
                synth(scope, this, INVALID_VALUE);
                return true;
            }
            let canvas = super::buffer_size(scope, this);
            edit(scope, this, |s| s.draw.drawing_buffer = Some(((w.max(1) as u32, h.max(1) as u32), canvas)));
        }
        _ => return false,
    }
    true
}

/// Is `attachment` a framebuffer attachment point (`ValidateFramebufferFuncParameters`)?
fn valid_fb_attachment(scope: &mut v8::PinScope, attachment: u32, v2: bool) -> bool {
    if matches!(attachment, DEPTH_ATTACHMENT | STENCIL_ATTACHMENT | DEPTH_STENCIL_ATTACHMENT) {
        return true;
    }
    let max = if v2 { device(scope, 0x8CDF, true, 8) as u32 } else { 1 };
    (COLOR_ATTACHMENT0..COLOR_ATTACHMENT0 + max.max(1)).contains(&attachment)
}

fn block_index_ok(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, program: u32, index: u32) -> bool {
    match ask(scope, this, GlQuery::ProgramParameter { program, pname: 0x8A36 }) {
        GlQueryResult::Int(n) => (index as i64) < n as i64,
        _ => false,
    }
}

/// A `sequence<DOMString>` argument; `None` = the binding layer's TypeError (a string is not a sequence).
fn string_seq_arg(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, member: &str, iface: &str, i: i32) -> Option<Vec<String>> {
    if !v.is_object() {
        throw_type_error(scope, &format!("Failed to execute '{member}' on '{iface}': The provided value cannot be converted to a sequence."));
        let _ = i;
        return None;
    }
    Some(string_seq(scope, v))
}

fn string_seq(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Vec<String> {
    let Ok(o) = v8::Local::<v8::Object>::try_from(v) else { return Vec::new() };
    let n = crate::binding::protos::get_prop(scope, o, "length").and_then(|l| l.uint32_value(scope)).unwrap_or(0);
    (0..n).filter_map(|i| o.get_index(scope, i)).map(|x| x.to_rust_string_lossy(scope)).collect()
}

/// `getFramebufferAttachmentParameter`, WebGL 1's and WebGL 2's.
fn framebuffer_attachment_parameter(
    scope: &mut v8::PinScope,
    this: v8::Local<v8::Object>,
    target: u32,
    attachment: u32,
    pname: u32,
    v2: bool,
    rv: &mut v8::ReturnValue,
) {
    let target_ok = if v2 { matches!(target, FRAMEBUFFER | READ_FRAMEBUFFER | DRAW_FRAMEBUFFER) } else { target == FRAMEBUFFER };
    let fb = edit(scope, this, |s| fb_for(s, target));
    let refuse = |scope: &mut v8::PinScope, rv: &mut v8::ReturnValue, code: u32| {
        synth(scope, this, code);
        rv.set(null_val(scope));
    };
    if !target_ok {
        return refuse(scope, rv, INVALID_ENUM);
    }
    if v2 && fb == 0 {
        if !matches!(attachment, BACK | DEPTH | STENCIL) {
            return refuse(scope, rv, INVALID_ENUM);
        }
        let attrs = edit(scope, this, |s| s.attrs.clone());
        let present = match attachment {
            DEPTH => attrs.depth,
            STENCIL => attrs.stencil,
            _ => true,
        };
        let sizes = match attachment {
            BACK => [8, 8, 8, if attrs.alpha { 8 } else { 0 }, 0, 0],
            DEPTH => [0, 0, 0, 0, if attrs.depth { 24 } else { 0 }, 0],
            _ => [0, 0, 0, 0, 0, if attrs.stencil { 8 } else { 0 }],
        };
        match pname {
            0x8CD0 => rv.set_uint32(if present { 0x8218 } else { 0 }), // FRAMEBUFFER_DEFAULT / NONE
            0x8212..=0x8217 => rv.set_int32(sizes[(pname - 0x8212) as usize]),
            0x8211 => rv.set_uint32(if present { UNSIGNED_NORMALIZED } else { 0 }),
            0x8210 => rv.set_uint32(LINEAR),
            _ => refuse(scope, rv, INVALID_ENUM),
        }
        return;
    }
    if !valid_fb_attachment(scope, attachment, v2) {
        return refuse(scope, rv, INVALID_ENUM);
    }
    if fb == 0 {
        return refuse(scope, rv, INVALID_OPERATION);
    }
    let lookup = if attachment == DEPTH_STENCIL_ATTACHMENT { DEPTH_ATTACHMENT } else { attachment };
    let (a, other) = edit(scope, this, |s| (s.draw.attach.get(&(fb, lookup)).copied(), s.draw.attach.get(&(fb, STENCIL_ATTACHMENT)).copied()));
    let Some(a) = a else {
        match pname {
            0x8CD0 => rv.set_uint32(0),
            0x8CD1 if v2 => rv.set(null_val(scope)),
            _ => refuse(scope, rv, if v2 { INVALID_OPERATION } else { INVALID_ENUM }),
        }
        return;
    };
    if attachment == DEPTH_STENCIL_ATTACHMENT && other.map(|o| o.id) != Some(a.id) {
        return refuse(scope, rv, INVALID_OPERATION);
    }
    let fmt = edit(scope, this, |s| attach_format(s, &a));
    let info = fmt.and_then(format_info).unwrap_or((0, 0, 0, 0, 0, 0, 0, LINEAR));
    let (sizes, ctype, enc) = ([info.0, info.1, info.2, info.3, info.4, info.5], info.6, info.7);
    match pname {
        0x8CD0 => rv.set_uint32(if a.texture { 0x1702 } else { 0x8D41 }), // TEXTURE / RENDERBUFFER
        0x8CD1 => match object_of(scope, this, a.id) {
            Some(o) => rv.set(o),
            None => rv.set(null_val(scope)),
        },
        0x8CD2 if a.texture => rv.set_int32(a.level),
        0x8CD3 if a.texture => rv.set_uint32(a.face),
        0x8CD4 if a.texture && v2 => rv.set_int32(a.layer),
        0x8212..=0x8217 if v2 => rv.set_int32(sizes[(pname - 0x8212) as usize]),
        0x8211 if v2 || edit(scope, this, |s| s.enabled.iter().any(|e| e.contains("color_buffer"))) => {
            if attachment == DEPTH_STENCIL_ATTACHMENT {
                return refuse(scope, rv, INVALID_OPERATION);
            }
            rv.set_uint32(ctype)
        }
        0x8210 if v2 || edit(scope, this, |s| s.enabled.contains("EXT_sRGB")) => rv.set_uint32(enc),
        _ => refuse(scope, rv, INVALID_ENUM),
    }
}

/// `clearBuffer{fv,iv,uiv,fi}`: Blink's `ValidateClearBuffer` (the buffer enum, the value count) and
/// GL's (the draw buffer index, the draw buffer's component type), then the clear itself through the
/// clear ops, the previous clear values put back after.
fn clear_buffer(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, member: &str, args: &v8::FunctionCallbackArguments, iface: &str) {
    let n = if member == "clearBufferfi" { 4 } else { 3 };
    if !need(scope, args, n, iface, member) {
        return;
    }
    let buffer = u32_arg(scope, args, 0);
    let drawbuffer = i32_arg(scope, args, 1);
    let allowed: &[u32] = match member {
        "clearBufferfv" => &[COLOR, DEPTH],
        "clearBufferiv" => &[COLOR, STENCIL],
        "clearBufferuiv" => &[COLOR],
        _ => &[DEPTH_STENCIL],
    };
    if !allowed.contains(&buffer) {
        synth(scope, this, INVALID_ENUM);
        return;
    }
    let values: Vec<f64> = if member == "clearBufferfi" {
        vec![args.get(2).number_value(scope).unwrap_or(0.0), args.get(3).number_value(scope).unwrap_or(0.0)]
    } else {
        let v = args.get(2);
        let Ok(o) = v8::Local::<v8::Object>::try_from(v) else {
            throw_type_error(scope, &format!("Failed to execute '{member}' on '{iface}': The provided value cannot be converted to a sequence."));
            return;
        };
        let len = crate::binding::protos::get_prop(scope, o, "length").and_then(|l| l.uint32_value(scope)).unwrap_or(0);
        let src = if args.length() > 3 { u32_arg(scope, args, 3) } else { 0 };
        if src > len {
            synth(scope, this, INVALID_VALUE);
            return;
        }
        (src..len).filter_map(|i| o.get_index(scope, i)).map(|x| x.number_value(scope).unwrap_or(0.0)).collect()
    };
    let need_n = if buffer == COLOR { 4 } else { 1 };
    if member != "clearBufferfi" && values.len() < need_n {
        synth(scope, this, INVALID_VALUE);
        return;
    }
    let max_draw = device(scope, 0x8824, true, 8) as i32;
    if drawbuffer < 0 || (buffer == COLOR && drawbuffer >= max_draw) || (buffer != COLOR && drawbuffer != 0) {
        synth(scope, this, INVALID_VALUE);
        return;
    }
    if buffer == COLOR {
        // The draw buffer's component type decides which spelling may clear it.
        let fb = edit(scope, this, |s| s.params.binding(0x8CA6));
        let ctype = if fb == 0 {
            UNSIGNED_NORMALIZED
        } else {
            let a = edit(scope, this, |s| s.draw.attach.get(&(fb, COLOR_ATTACHMENT0 + drawbuffer as u32)).copied());
            match a {
                None => return,
                Some(a) => edit(scope, this, |s| attach_format(s, &a)).and_then(format_info).map(|i| i.6).unwrap_or(UNSIGNED_NORMALIZED),
            }
        };
        let ok = match member {
            "clearBufferiv" => ctype == INT,
            "clearBufferuiv" => ctype == UNSIGNED_INT,
            _ => !matches!(ctype, INT | UNSIGNED_INT),
        };
        if !ok {
            synth(scope, this, INVALID_OPERATION);
            return;
        }
        if member == "clearBufferfv" && drawbuffer == 0 {
            let (old, _, _) = edit(scope, this, |s| s.params.clear_values());
            push(scope, this, GlOp::ClearColor { r: values[0] as f32, g: values[1] as f32, b: values[2] as f32, a: values[3] as f32 });
            push(scope, this, GlOp::Clear { mask: 0x4000 });
            push(scope, this, GlOp::ClearColor { r: old[0], g: old[1], b: old[2], a: old[3] });
        }
        return;
    }
    let (_, old_d, old_s) = edit(scope, this, |s| s.params.clear_values());
    if buffer == DEPTH || buffer == DEPTH_STENCIL {
        push(scope, this, GlOp::ClearDepth { depth: values[0] as f32 });
    }
    if buffer == STENCIL || buffer == DEPTH_STENCIL {
        let sv = if buffer == STENCIL { values[0] } else { values[1] };
        push(scope, this, GlOp::ClearStencil { s: sv as i32 });
    }
    let mask = match buffer {
        DEPTH => 0x0100,
        STENCIL => 0x0400,
        _ => 0x0500,
    };
    push(scope, this, GlOp::Clear { mask });
    push(scope, this, GlOp::ClearDepth { depth: old_d });
    push(scope, this, GlOp::ClearStencil { s: old_s });
}

/// `getVertexAttrib`'s answers this file keeps: `VERTEX_ATTRIB_ARRAY_INTEGER`, and the current value
/// of an attribute an integer call set (an `Int32Array` / `Uint32Array`). `false` = not one of them.
pub(super) fn vertex_attrib(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, index: u32, pname: u32, v2: bool, rv: &mut v8::ReturnValue) -> bool {
    if !v2 {
        return false;
    }
    match pname {
        0x88FD => {
            let b = edit(scope, this, |s| s.draw.attrib_integer.get(&index).copied().unwrap_or(false));
            rv.set_bool(b);
            true
        }
        0x8626 => {
            let Some((kind, v)) = edit(scope, this, |s| s.draw.attrib_int.get(&index).copied()) else { return false };
            let arr = if kind == 2 {
                super::new_u32_array(scope, &[v[0] as u32, v[1] as u32, v[2] as u32, v[3] as u32])
            } else {
                super::new_i32_array(scope, &[v[0] as i32, v[1] as i32, v[2] as i32, v[3] as i32])
            };
            match arr {
                Some(a) => rv.set(a),
                None => rv.set(null_val(scope)),
            }
            true
        }
        _ => false,
    }
}

// ── uniforms ────────────────────────────────────────────────────────────────────────────────────

/// Does a `uniform*` spelling set a uniform of GL type `ty`? (OpenGL ES 3.0 §2.12.6: the `f`, `i` and
/// `ui` spellings each set their own type and `bool`, `1i` / `1iv` a sampler, a matrix spelling only
/// its own matrix.)
fn uniform_type_ok(value: &GlUniformValue, ty: u32) -> bool {
    let bool_of = |n: u8| match n {
        1 => 0x8B56,
        2 => 0x8B57,
        3 => 0x8B58,
        _ => 0x8B59,
    };
    let sampler = matches!(
        ty,
        0x8B5E | 0x8B5F | 0x8B60 | 0x8B62 | 0x8DC1 | 0x8DC4 | 0x8DC5 | 0x8DCA | 0x8DCB | 0x8DCC | 0x8DCF | 0x8DD2 | 0x8DD3 | 0x8DD4 | 0x8DD7
    );
    match value {
        GlUniformValue::Float { components, .. } => {
            let own = match components {
                1 => 0x1406,
                2 => 0x8B50,
                3 => 0x8B51,
                _ => 0x8B52,
            };
            ty == own || ty == bool_of(*components)
        }
        GlUniformValue::Int { components, .. } => {
            let own = match components {
                1 => 0x1404,
                2 => 0x8B53,
                3 => 0x8B54,
                _ => 0x8B55,
            };
            ty == own || ty == bool_of(*components) || (*components == 1 && sampler)
        }
        GlUniformValue::UInt { components, .. } => {
            let own = match components {
                1 => 0x1405,
                2 => 0x8DC6,
                3 => 0x8DC7,
                _ => 0x8DC8,
            };
            ty == own || ty == bool_of(*components)
        }
        GlUniformValue::Matrix { rows, cols, .. } => {
            let own = match (cols, rows) {
                (2, 2) => 0x8B5A,
                (3, 3) => 0x8B5B,
                (4, 4) => 0x8B5C,
                (2, 3) => 0x8B65,
                (2, 4) => 0x8B66,
                (3, 2) => 0x8B67,
                (3, 4) => 0x8B68,
                (4, 2) => 0x8B69,
                _ => 0x8B6A,
            };
            ty == own
        }
    }
}

/// The checks a `uniform*` call goes through before it is recorded (Blink's
/// `ValidateUniformParameters` / `ValidateUniformMatrixParameters`, then GL's type and count checks),
/// with WebGL 2's `srcOffset` / `srcLength` applied to the value. `Some(value)` = record it.
pub(super) fn check_uniform(
    scope: &mut v8::PinScope,
    this: v8::Local<v8::Object>,
    member: &str,
    args: &v8::FunctionCallbackArguments,
    program: u32,
    name: &str,
    value: GlUniformValue,
    v2: bool,
) -> Option<GlUniformValue> {
    let current = edit(scope, this, |s| s.params.binding(0x8B8D));
    if current == 0 || current != program {
        synth(scope, this, INVALID_OPERATION);
        return None;
    }
    let is_vector = member.ends_with('v');
    let per = match &value {
        GlUniformValue::Float { components, .. } | GlUniformValue::Int { components, .. } | GlUniformValue::UInt { components, .. } => *components as usize,
        GlUniformValue::Matrix { rows, cols, .. } => *rows as usize * *cols as usize,
    };
    // WebGL 2's `(data, srcOffset, srcLength)` on the array forms.
    let data_at = if member.starts_with("uniformMatrix") { 2 } else { 1 };
    let (src_off, src_len) = if v2 && is_vector && args.length() > data_at + 1 {
        (u32_arg(scope, args, data_at + 1) as usize, if args.length() > data_at + 2 { u32_arg(scope, args, data_at + 2) as usize } else { 0 })
    } else {
        (0, 0)
    };
    let slice = |len: usize| -> Option<(usize, usize)> {
        if src_off > len {
            return None;
        }
        let n = if src_len == 0 { len - src_off } else { src_len };
        (src_off + n <= len).then_some((src_off, n))
    };
    let value = match value {
        GlUniformValue::Float { components, values } if is_vector => {
            let (o, n) = slice(values.len()).or_else(|| {
                synth(scope, this, INVALID_VALUE);
                None
            })?;
            GlUniformValue::Float { components, values: values[o..o + n].to_vec() }
        }
        GlUniformValue::Int { components, values } if is_vector => {
            let (o, n) = slice(values.len()).or_else(|| {
                synth(scope, this, INVALID_VALUE);
                None
            })?;
            GlUniformValue::Int { components, values: values[o..o + n].to_vec() }
        }
        GlUniformValue::UInt { components, values } if is_vector => {
            let (o, n) = slice(values.len()).or_else(|| {
                synth(scope, this, INVALID_VALUE);
                None
            })?;
            GlUniformValue::UInt { components, values: values[o..o + n].to_vec() }
        }
        GlUniformValue::Matrix { rows, cols, transpose, values } => {
            let (o, n) = slice(values.len()).or_else(|| {
                synth(scope, this, INVALID_VALUE);
                None
            })?;
            if transpose && !v2 {
                synth(scope, this, INVALID_VALUE);
                return None;
            }
            GlUniformValue::Matrix { rows, cols, transpose, values: values[o..o + n].to_vec() }
        }
        other => other,
    };
    let len = match &value {
        GlUniformValue::Float { values, .. } => values.len(),
        GlUniformValue::Int { values, .. } => values.len(),
        GlUniformValue::UInt { values, .. } => values.len(),
        GlUniformValue::Matrix { values, .. } => values.len(),
    };
    if (is_vector || member.starts_with("uniformMatrix")) && (len < per || len % per != 0) {
        synth(scope, this, INVALID_VALUE);
        return None;
    }
    let ty = match ask(scope, this, GlQuery::UniformType { program, name: name.to_string() }) {
        GlQueryResult::UInt(t) => t,
        _ => return Some(value),
    };
    if !uniform_type_ok(&value, ty) {
        synth(scope, this, INVALID_OPERATION);
        return None;
    }
    // More than one element for a uniform that is not an array.
    if len / per.max(1) > 1 && !uniform_is_array(scope, this, program, name) {
        synth(scope, this, INVALID_OPERATION);
        return None;
    }
    Some(value)
}

fn uniform_is_array(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, program: u32, name: &str) -> bool {
    if name.contains('[') {
        return true;
    }
    let idx = match ask(scope, this, GlQuery::UniformIndices { program, names: vec![name.to_string()] }) {
        GlQueryResult::UInts(v) => v.first().copied().unwrap_or(u32::MAX),
        _ => u32::MAX,
    };
    if idx == u32::MAX {
        return false;
    }
    matches!(ask(scope, this, GlQuery::ActiveUniforms { program, indices: vec![idx], pname: 0x8A38 }), GlQueryResult::Ints(v) if v.first().copied().unwrap_or(1) > 1)
}
