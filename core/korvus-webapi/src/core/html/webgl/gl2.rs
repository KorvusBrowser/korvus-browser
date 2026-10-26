//! The WebGL 2 object state Blink keeps on its side of the command buffer -- samplers, queries, syncs,
//! the indexed buffer bindings, buffer contents and immutable texture storage
//! (the Chrome oracle ratchet).
//!
//! These calls were no-ops answering `null` (`getSamplerParameter`, `getSyncParameter`,
//! `getIndexedParameter`, `getQueryParameter`) or recording nothing (`samplerParameteri`,
//! `beginQuery`, `texStorage2D`, `getBufferSubData`), so a page that made a sampler and read its
//! `TEXTURE_MIN_FILTER` got `null` where every Chrome answers `NEAREST_MIPMAP_LINEAR`, and no error was
//! ever raised for a bad enum. Each body here is `WebGL2RenderingContextBase`'s (Chromium 150,
//! `modules/webgl/webgl2_rendering_context_base.cc`) plus the checks GL itself makes behind it
//! (ANGLE's), measured against stock Chrome 150 with `core/korvus-runtime/tests/fixtures/webgl2/`.
//!
//! None of this draws: the software pipeline samples with each texture's own parameters and has no
//! occlusion queries, so these objects carry exactly the state a page can read back.

use deno_core::v8;
use spec::GlOp;
use std::collections::{HashMap, HashSet};

use super::{edit, null_val, obj_id, object_of};
use crate::binding::protos::throw_type_error;

const INVALID_ENUM: u32 = 0x0500;
const INVALID_VALUE: u32 = 0x0501;
const INVALID_OPERATION: u32 = 0x0502;

const ARRAY_BUFFER: u32 = 0x8892;
const ELEMENT_ARRAY_BUFFER: u32 = 0x8893;
const UNIFORM_BUFFER: u32 = 0x8A11;
const TRANSFORM_FEEDBACK_BUFFER: u32 = 0x8C8E;

const TEXTURE_2D: u32 = 0x0DE1;
const TEXTURE_CUBE_MAP: u32 = 0x8513;
const TEXTURE_3D: u32 = 0x806F;
const TEXTURE_2D_ARRAY: u32 = 0x8C1A;

/// One sampler's parameters, at GL's initial values.
#[derive(Clone, Debug)]
pub(crate) struct Sampler {
    ints: HashMap<u32, u32>,
    min_lod: f32,
    max_lod: f32,
}

impl Default for Sampler {
    fn default() -> Self {
        let ints = [
            (0x2801, 0x2702), // TEXTURE_MIN_FILTER: NEAREST_MIPMAP_LINEAR
            (0x2800, 0x2601), // TEXTURE_MAG_FILTER: LINEAR
            (0x2802, 0x2901), // TEXTURE_WRAP_S: REPEAT
            (0x2803, 0x2901), // TEXTURE_WRAP_T
            (0x8072, 0x2901), // TEXTURE_WRAP_R
            (0x884C, 0),      // TEXTURE_COMPARE_MODE: NONE
            (0x884D, 0x0203), // TEXTURE_COMPARE_FUNC: LEQUAL
        ]
        .into_iter()
        .collect();
        Sampler { ints, min_lod: -1000.0, max_lod: 1000.0 }
    }
}

/// One indexed binding point: the buffer and the range `bindBufferRange` gave (0, 0 for `bindBufferBase`).
#[derive(Clone, Copy, Debug, Default)]
struct Indexed {
    buffer: u32,
    start: i64,
    size: i64,
}

#[derive(Default)]
pub(crate) struct Gl2State {
    samplers: HashMap<u32, Sampler>,
    /// A query's target, once `beginQuery` has given it one.
    query_target: HashMap<u32, u32>,
    /// The active `ANY_SAMPLES_PASSED[_CONSERVATIVE]` query and `TRANSFORM_FEEDBACK_PRIMITIVES_WRITTEN` one.
    active_occlusion: Option<u32>,
    active_tf: Option<u32>,
    /// Queries whose result a task after `endQuery` has made available (Blink's `can_update_availability_`).
    available: HashSet<u32>,
    syncs: HashSet<u32>,
    deleted: HashSet<u32>,
    ub: Vec<Indexed>,
    tf: Vec<Indexed>,
    /// Buffer -> its contents.
    mem: HashMap<u32, Vec<u8>>,
    /// Texture -> (immutable internal format, levels) after `texStorage*`.
    pub(crate) immutable: HashMap<u32, (u32, u32)>,
    /// (texture, level) -> (width, height, depth) of a defined image.
    pub(crate) levels: HashMap<(u32, i32), (i64, i64, i64)>,
}

impl Gl2State {
    /// The buffer bound at `TRANSFORM_FEEDBACK_BUFFER` index `i` (0 for none).
    pub(crate) fn tf_binding(&self, i: u32) -> u32 {
        self.tf.get(i as usize).map(|b| b.buffer).unwrap_or(0)
    }
}

// ── argument helpers ─────────────────────────────────────────────────────────────────────────────

/// A non-nullable WebGL object argument: the binding layer's TypeError for anything that is not one,
/// then Blink's `ValidateWebGLObject` -- an object of another context or one already deleted is
/// `INVALID_OPERATION`. `Ok(Some(id))` = valid, `Ok(None)` = an error was synthesized, `Err` = thrown.
fn object_arg(
    scope: &mut v8::PinScope,
    this: v8::Local<v8::Object>,
    args: &v8::FunctionCallbackArguments,
    i: i32,
    iface: &str,
    method: &str,
    holder: &str,
    nullable: bool,
) -> Result<Option<u32>, ()> {
    let v = args.get(i);
    if nullable && v.is_null_or_undefined() {
        return Ok(Some(0));
    }
    if !crate::binding::protos::implements(scope, v, iface) {
        throw_type_error(scope, &format!("Failed to execute '{method}' on '{holder}': parameter {} is not of type '{iface}'.", i + 1));
        return Err(());
    }
    let id = obj_id(scope, v);
    let mine = object_of(scope, this, id).is_some_and(|o| o.strict_equals(v));
    if !mine || edit(scope, this, |s| s.gl2.deleted.contains(&id) || s.params.is_deleted(id)) {
        synth(scope, this, INVALID_OPERATION);
        return Ok(None);
    }
    Ok(Some(id))
}

fn synth(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, code: u32) {
    edit(scope, this, |s| s.synthesize(code));
}

/// A WebIDL `long long` / `GLintptr` argument.
fn i64_arg(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, i: i32) -> i64 {
    let n = args.get(i).number_value(scope).unwrap_or(0.0);
    if n.is_finite() { n.trunc() as i64 } else { 0 }
}

fn u32_arg(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, i: i32) -> u32 {
    args.get(i).uint32_value(scope).unwrap_or(0)
}

fn i32_arg(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, i: i32) -> i32 {
    args.get(i).int32_value(scope).unwrap_or(0)
}

/// The device limit `pname`, from the persona's WebGL 2 parameters.
fn limit(scope: &mut v8::PinScope, pname: u32, dflt: i64) -> i64 {
    super::device_number(scope, pname, true).map(|n| n as i64).unwrap_or(dflt)
}

/// Is this one of WebGL 2's SIZED internal formats (the ones `texStorage*` accepts)?
fn sized_format(f: u32) -> bool {
    matches!(
        f,
        0x8229 | 0x8F94 | 0x822D | 0x822E | 0x8232 | 0x8231 | 0x8234 | 0x8233 | 0x8236 | 0x8235 // R*
            | 0x822B | 0x8F95 | 0x822F | 0x8230 | 0x8238 | 0x8237 | 0x823A | 0x8239 | 0x823C | 0x823B // RG*
            | 0x8051 | 0x8C41 | 0x8D62 | 0x8F96 | 0x8C3A | 0x8C3D | 0x881B | 0x8815 | 0x8D7D | 0x8D8F
            | 0x8D77 | 0x8D89 | 0x8D71 | 0x8D83 // RGB*
            | 0x8058 | 0x8C43 | 0x8F97 | 0x8057 | 0x8056 | 0x8059 | 0x881A | 0x8814 | 0x8D7C | 0x8D8E
            | 0x906F | 0x8D76 | 0x8D88 | 0x8D82 | 0x8D70 // RGBA*
            | 0x81A5 | 0x81A6 | 0x8CAC | 0x88F0 | 0x8CAD // depth / stencil
    )
}

// ── the calls ────────────────────────────────────────────────────────────────────────────────────

/// The WebGL 2 calls this file answers. `false` = not one of them.
pub(super) fn call(
    scope: &mut v8::PinScope,
    this: v8::Local<v8::Object>,
    member: &str,
    args: &v8::FunctionCallbackArguments,
    rv: &mut v8::ReturnValue,
    holder: &str,
) -> bool {
    match member {
        "samplerParameteri" | "samplerParameterf" => {
            let Ok(Some(s)) = object_arg(scope, this, args, 0, "WebGLSampler", member, holder, false) else { return true };
            let pname = u32_arg(scope, args, 1);
            let is_float = member == "samplerParameterf";
            let f = args.get(2).number_value(scope).unwrap_or(0.0) as f32;
            // Blink validates the parameter as GLint -- a float saturates toward zero first.
            let p: i64 = if is_float { if f.is_nan() { 0 } else { f.clamp(i32::MIN as f32, i32::MAX as f32) as i64 } } else { i32_arg(scope, args, 2) as i64 };
            let ok_value = match pname {
                0x813A | 0x813B => true, // TEXTURE_MIN_LOD / TEXTURE_MAX_LOD
                0x884D => matches!(p, 0x0200..=0x0207),
                0x884C => matches!(p, 0 | 0x884E),
                0x2800 => matches!(p, 0x2600 | 0x2601),
                0x2801 => matches!(p, 0x2600 | 0x2601 | 0x2700..=0x2703),
                0x8072 | 0x2802 | 0x2803 => matches!(p, 0x812F | 0x8370 | 0x2901),
                _ => {
                    synth(scope, this, INVALID_ENUM);
                    return true;
                }
            };
            if !ok_value {
                synth(scope, this, INVALID_ENUM);
                return true;
            }
            edit(scope, this, |st| {
                let smp = st.gl2.samplers.entry(s).or_default();
                match pname {
                    0x813A => smp.min_lod = if is_float { f } else { p as f32 },
                    0x813B => smp.max_lod = if is_float { f } else { p as f32 },
                    // GL rounds a float parameter of an integer state to the nearest integer.
                    _ => {
                        let v = if is_float { f.round() as i64 } else { p };
                        smp.ints.insert(pname, v as u32);
                    }
                }
            });
        }
        "getSamplerParameter" => {
            let s = match object_arg(scope, this, args, 0, "WebGLSampler", member, holder, false) {
                Err(()) => return true,
                Ok(None) => {
                    rv.set(null_val(scope));
                    return true;
                }
                Ok(Some(s)) => s,
            };
            let pname = u32_arg(scope, args, 1);
            let smp = edit(scope, this, |st| st.gl2.samplers.get(&s).cloned().unwrap_or_default());
            match pname {
                0x813A => rv.set(v8::Number::new(scope, smp.min_lod as f64).into()),
                0x813B => rv.set(v8::Number::new(scope, smp.max_lod as f64).into()),
                p if smp.ints.contains_key(&p) => rv.set_uint32(smp.ints[&p]),
                _ => {
                    synth(scope, this, INVALID_ENUM);
                    rv.set(null_val(scope));
                }
            }
        }
        "deleteSampler" | "deleteQuery" | "deleteSync" => {
            let id = obj_id(scope, args.get(0));
            if id != 0 {
                edit(scope, this, |st| {
                    st.gl2.deleted.insert(id);
                    st.gl2.samplers.remove(&id);
                    st.gl2.syncs.remove(&id);
                    if st.gl2.active_occlusion == Some(id) {
                        st.gl2.active_occlusion = None;
                    }
                    if st.gl2.active_tf == Some(id) {
                        st.gl2.active_tf = None;
                    }
                });
            }
            return false; // webgl.rs drops the identity from `aux` as before
        }

        // ── queries ──
        "beginQuery" => {
            let target = u32_arg(scope, args, 0);
            let Ok(Some(q)) = object_arg(scope, this, args, 1, "WebGLQuery", member, holder, false) else { return true };
            let e = edit(scope, this, |st| {
                let g = &mut st.gl2;
                if let Some(&t) = g.query_target.get(&q) {
                    if t != target {
                        return Some(INVALID_OPERATION);
                    }
                }
                match target {
                    0x8C2F | 0x8D6A => {
                        if g.active_occlusion.is_some() {
                            return Some(INVALID_OPERATION);
                        }
                        g.active_occlusion = Some(q);
                    }
                    0x8C88 => {
                        if g.active_tf.is_some() {
                            return Some(INVALID_OPERATION);
                        }
                        g.active_tf = Some(q);
                    }
                    _ => return Some(INVALID_ENUM),
                }
                g.query_target.entry(q).or_insert(target);
                g.available.remove(&q);
                st.bound.insert(q);
                None
            });
            if let Some(e) = e {
                synth(scope, this, e);
            }
        }
        "endQuery" => {
            let target = u32_arg(scope, args, 0);
            let r = edit(scope, this, |st| {
                let g = &mut st.gl2;
                match target {
                    0x8C2F | 0x8D6A => match g.active_occlusion {
                        Some(q) if g.query_target.get(&q) == Some(&target) => {
                            g.active_occlusion = None;
                            Ok(q)
                        }
                        _ => Err(INVALID_OPERATION),
                    },
                    0x8C88 => g.active_tf.take().ok_or(INVALID_OPERATION),
                    _ => Err(INVALID_ENUM),
                }
            });
            match r {
                Err(e) => synth(scope, this, e),
                Ok(q) => schedule_availability(scope, this, q),
            }
        }
        "getQuery" => {
            let (target, pname) = (u32_arg(scope, args, 0), u32_arg(scope, args, 1));
            if pname != 0x8865 {
                synth(scope, this, INVALID_ENUM);
                rv.set(null_val(scope));
                return true;
            }
            let r = edit(scope, this, |st| match target {
                0x8C2F | 0x8D6A => Ok(st.gl2.active_occlusion.filter(|q| st.gl2.query_target.get(q) == Some(&target))),
                0x8C88 => Ok(st.gl2.active_tf),
                _ => Err(()),
            });
            match r {
                Err(()) => {
                    synth(scope, this, INVALID_ENUM);
                    rv.set(null_val(scope));
                }
                Ok(Some(q)) => match object_of(scope, this, q) {
                    Some(o) => rv.set(o),
                    None => rv.set(null_val(scope)),
                },
                Ok(None) => rv.set(null_val(scope)),
            }
        }
        "getQueryParameter" => {
            let q = match object_arg(scope, this, args, 0, "WebGLQuery", member, holder, false) {
                Err(()) => return true,
                Ok(None) => {
                    rv.set(null_val(scope));
                    return true;
                }
                Ok(Some(q)) => q,
            };
            let pname = u32_arg(scope, args, 1);
            let (begun, active, available) = edit(scope, this, |st| {
                let g = &st.gl2;
                (g.query_target.contains_key(&q), g.active_occlusion == Some(q) || g.active_tf == Some(q), g.available.contains(&q))
            });
            if !begun || active {
                synth(scope, this, INVALID_OPERATION);
                rv.set(null_val(scope));
                return true;
            }
            match pname {
                // QUERY_RESULT: nothing was drawn under the query, so no sample passed and no
                // primitive was written.
                0x8866 => rv.set_uint32(0),
                0x8867 => rv.set_bool(available),
                _ => {
                    synth(scope, this, INVALID_ENUM);
                    rv.set(null_val(scope));
                }
            }
        }

        // ── syncs ──
        "fenceSync" => {
            let (cond, flags) = (u32_arg(scope, args, 0), u32_arg(scope, args, 1));
            if cond != 0x9117 {
                synth(scope, this, INVALID_ENUM);
                rv.set(null_val(scope));
                return true;
            }
            if flags != 0 {
                synth(scope, this, INVALID_VALUE);
                rv.set(null_val(scope));
                return true;
            }
            return false; // webgl.rs mints the object; `note_sync` records it
        }
        "getSyncParameter" => {
            match object_arg(scope, this, args, 0, "WebGLSync", member, holder, false) {
                Err(()) => return true,
                Ok(None) => {
                    rv.set(null_val(scope));
                    return true;
                }
                Ok(Some(_)) => {}
            }
            match u32_arg(scope, args, 1) {
                0x9112 => rv.set_uint32(0x9116),                // OBJECT_TYPE: SYNC_FENCE
                0x9113 => rv.set_uint32(0x9117),                // SYNC_CONDITION
                0x9115 => rv.set_uint32(0),                     // SYNC_FLAGS
                0x9114 => rv.set_uint32(0x9118),                // SYNC_STATUS: UNSIGNALED until a task has run
                _ => {
                    synth(scope, this, INVALID_ENUM);
                    rv.set(null_val(scope));
                }
            }
        }
        "clientWaitSync" => {
            let Ok(Some(_y)) = object_arg(scope, this, args, 0, "WebGLSync", member, holder, false) else {
                rv.set_uint32(0x911D); // WAIT_FAILED
                return true;
            };
            let flags = u32_arg(scope, args, 1);
            // `GLuint64 timeout`: WebIDL's ToUint64 takes a negative modulo 2^64, so `-1` is enormous.
            let timeout = args.get(2).number_value(scope).unwrap_or(0.0);
            let timeout = if timeout.is_finite() { timeout.trunc() } else { 0.0 };
            if timeout != 0.0 {
                // MAX_CLIENT_WAIT_TIMEOUT_WEBGL is 0 in Chrome
                synth(scope, this, INVALID_OPERATION);
                rv.set_uint32(0x911D);
                return true;
            }
            if !(flags == 0 || flags == 1) {
                synth(scope, this, INVALID_VALUE);
                rv.set_uint32(0x911D);
                return true;
            }
            // A fence is not seen to signal inside the task that made it -- Blink polls GL at most once
            // per frame (measured: TIMEOUT_EXPIRED right after fenceSync).
            rv.set_uint32(0x911B); // TIMEOUT_EXPIRED
        }
        "waitSync" => {
            let Ok(Some(_y)) = object_arg(scope, this, args, 0, "WebGLSync", member, holder, false) else { return true };
            let flags = u32_arg(scope, args, 1);
            let timeout = args.get(2).number_value(scope).unwrap_or(0.0);
            if flags != 0 || timeout != -1.0 {
                synth(scope, this, INVALID_VALUE);
            }
        }

        // ── indexed buffer bindings ──
        "bindBufferBase" | "bindBufferRange" => {
            let target = u32_arg(scope, args, 0);
            let index = u32_arg(scope, args, 1);
            let Ok(Some(buffer)) = object_arg(scope, this, args, 2, "WebGLBuffer", member, holder, true) else { return true };
            let range = member == "bindBufferRange";
            let (offset, size) = if range { (i64_arg(scope, args, 3), i64_arg(scope, args, 4)) } else { (0, 0) };
            if range && (offset < 0 || size < 0 || offset > i32::MAX as i64 || size > i32::MAX as i64) {
                synth(scope, this, INVALID_VALUE);
                return true;
            }
            let max = match target {
                UNIFORM_BUFFER => limit(scope, 0x8A2F, 24),
                TRANSFORM_FEEDBACK_BUFFER => limit(scope, 0x8C8B, 4),
                _ => {
                    synth(scope, this, INVALID_ENUM);
                    return true;
                }
            };
            let align = if target == UNIFORM_BUFFER { limit(scope, 0x8A34, 256).max(1) } else { 4 };
            // the binding-point checks Blink makes first (element-array buffers can serve no other target)
            let e = edit(scope, this, |st| st.params.bind_buffer_base(target, buffer));
            if let Some(e) = e {
                synth(scope, this, e);
                return true;
            }
            if index as i64 >= max {
                synth(scope, this, INVALID_VALUE);
                return true;
            }
            super::bound(scope, this, buffer);
            // Blink records the indexed binding, then GL checks the range.
            let gl_err = range && buffer != 0 && (size <= 0 || offset % align != 0 || (target == TRANSFORM_FEEDBACK_BUFFER && size % 4 != 0));
            edit(scope, this, |st| {
                let list = if target == UNIFORM_BUFFER { &mut st.gl2.ub } else { &mut st.gl2.tf };
                if list.len() < max as usize {
                    list.resize(max as usize, Indexed::default());
                }
                let slot = &mut list[index as usize];
                slot.buffer = buffer;
                if !gl_err {
                    slot.start = if range { offset } else { 0 };
                    slot.size = if range { size } else { 0 };
                }
            });
            if gl_err {
                synth(scope, this, INVALID_VALUE);
            }
        }
        "getIndexedParameter" => {
            let (target, index) = (u32_arg(scope, args, 0), u32_arg(scope, args, 1));
            let (kind, max) = match target {
                0x8C8F | 0x8C84 | 0x8C85 => (TRANSFORM_FEEDBACK_BUFFER, limit(scope, 0x8C8B, 4)),
                0x8A28 | 0x8A29 | 0x8A2A => (UNIFORM_BUFFER, limit(scope, 0x8A2F, 24)),
                _ => {
                    synth(scope, this, INVALID_ENUM);
                    rv.set(null_val(scope));
                    return true;
                }
            };
            let binding = matches!(target, 0x8C8F | 0x8A28);
            if index as i64 >= max {
                synth(scope, this, INVALID_VALUE);
                // Blink answers null for a binding; a START/SIZE read is GL's, which leaves its -1.
                if binding {
                    rv.set(null_val(scope));
                } else {
                    rv.set(v8::Number::new(scope, -1.0).into());
                }
                return true;
            }
            let slot = edit(scope, this, |st| {
                let list = if kind == UNIFORM_BUFFER { &st.gl2.ub } else { &st.gl2.tf };
                list.get(index as usize).copied().unwrap_or_default()
            });
            match target {
                0x8C8F | 0x8A28 => match object_of(scope, this, slot.buffer) {
                    Some(o) => rv.set(o),
                    None => rv.set(null_val(scope)),
                },
                0x8C84 | 0x8A29 => rv.set(v8::Number::new(scope, slot.start as f64).into()),
                _ => rv.set(v8::Number::new(scope, slot.size as f64).into()),
            }
        }

        // ── buffer contents ──
        "getBufferSubData" => {
            let target = u32_arg(scope, args, 0);
            let src = i64_arg(scope, args, 1);
            let dst = args.get(2);
            let Ok(view) = v8::Local::<v8::ArrayBufferView>::try_from(dst) else {
                throw_type_error(scope, &format!("Failed to execute 'getBufferSubData' on '{holder}': parameter 3 is not of type 'ArrayBufferView'."));
                return true;
            };
            let dst_offset = i64_arg(scope, args, 3);
            let length = u32_arg(scope, args, 4) as i64;
            if src < 0 {
                synth(scope, this, INVALID_VALUE);
                return true;
            }
            let Some(buffer) = buffer_for(scope, this, target) else { return true };
            let elem = element_size(view);
            let view_len = view.byte_length() as i64 / elem;
            if dst_offset > view_len {
                synth(scope, this, INVALID_VALUE);
                return true;
            }
            let count = if length == 0 { view_len - dst_offset } else { length };
            if length != 0 && length > view_len - dst_offset {
                synth(scope, this, INVALID_VALUE);
                return true;
            }
            let bytes = (count * elem) as usize;
            let data = edit(scope, this, |st| {
                let mem = st.gl2.mem.get(&buffer).map(|m| m.as_slice()).unwrap_or(&[]);
                let s = src as usize;
                (s + bytes <= mem.len()).then(|| mem[s..s + bytes].to_vec())
            });
            let Some(data) = data else {
                synth(scope, this, INVALID_VALUE);
                return true;
            };
            if bytes == 0 {
                return true;
            }
            let at = view.byte_offset() + (dst_offset * elem) as usize;
            let Some(ab) = view.buffer(scope) else { return true };
            if let Some(base) = ab.data() {
                // SAFETY: the view's backing store holds `byte_offset + byte_length` bytes and this
                // writes `bytes` of them starting inside the view.
                unsafe {
                    std::ptr::copy_nonoverlapping(data.as_ptr(), (base.as_ptr() as *mut u8).add(at), bytes);
                }
            }
        }
        "copyBufferSubData" => {
            let (rt, wt) = (u32_arg(scope, args, 0), u32_arg(scope, args, 1));
            let (ro, wo, size) = (i64_arg(scope, args, 2), i64_arg(scope, args, 3), i64_arg(scope, args, 4));
            if ro < 0 || wo < 0 || size < 0 {
                synth(scope, this, INVALID_VALUE);
                return true;
            }
            let Some(rb) = buffer_for(scope, this, rt) else { return true };
            let Some(wb) = buffer_for(scope, this, wt) else { return true };
            let e = edit(scope, this, |st| {
                let rl = st.gl2.mem.get(&rb).map(|m| m.len()).unwrap_or(0) as i64;
                let wl = st.gl2.mem.get(&wb).map(|m| m.len()).unwrap_or(0) as i64;
                if ro + size > rl || wo + size > wl || (rb == wb && ro < wo + size && wo < ro + size) {
                    return Some(INVALID_VALUE);
                }
                let chunk = st.gl2.mem.get(&rb).map(|m| m[ro as usize..(ro + size) as usize].to_vec()).unwrap_or_default();
                if let Some(w) = st.gl2.mem.get_mut(&wb) {
                    w[wo as usize..(wo + size) as usize].copy_from_slice(&chunk);
                }
                None
            });
            if let Some(e) = e {
                synth(scope, this, e);
            }
        }

        // ── immutable texture storage ──
        "texStorage2D" | "texStorage3D" => {
            let three = member == "texStorage3D";
            let target = u32_arg(scope, args, 0);
            let levels = i32_arg(scope, args, 1);
            let format = u32_arg(scope, args, 2);
            let w = i32_arg(scope, args, 3) as i64;
            let h = i32_arg(scope, args, 4) as i64;
            let d = if three { i32_arg(scope, args, 5) as i64 } else { 1 };
            let target_ok = if three { matches!(target, TEXTURE_3D | TEXTURE_2D_ARRAY) } else { matches!(target, TEXTURE_2D | TEXTURE_CUBE_MAP) };
            if !target_ok {
                synth(scope, this, INVALID_ENUM);
                return true;
            }
            let tex = edit(scope, this, |st| st.params.texture_for(target));
            if tex == 0 {
                synth(scope, this, INVALID_OPERATION);
                return true;
            }
            if !sized_format(format) {
                synth(scope, this, INVALID_ENUM);
                return true;
            }
            if levels <= 0 || w <= 0 || h <= 0 || d <= 0 {
                synth(scope, this, INVALID_VALUE);
                return true;
            }
            let span = if target == TEXTURE_3D { w.max(h).max(d) } else { w.max(h) };
            let max_levels = 64 - (span as u64).leading_zeros() as i64;
            let e = edit(scope, this, |st| {
                if (levels as i64) > max_levels || st.gl2.immutable.contains_key(&tex) {
                    return Some(INVALID_OPERATION);
                }
                st.gl2.immutable.insert(tex, (format, levels as u32));
                super::gl2b::note_storage(st, target, levels, format);
                for l in 0..levels {
                    let dd = if target == TEXTURE_3D { (d >> l).max(1) } else { d };
                    st.gl2.levels.insert((tex, l), ((w >> l).max(1), (h >> l).max(1), dd));
                }
                // the storage itself, which a GPU context must have before any upload into it
                st.ops.push(GlOp::TexStorage {
                    target,
                    levels,
                    internal_format: format,
                    width: w as i32,
                    height: h as i32,
                    depth: if three { d as i32 } else { 0 },
                });
                None
            });
            if let Some(e) = e {
                synth(scope, this, e);
            }
        }
        _ => return false,
    }
    true
}

/// The buffer bound to `target`, or the error Blink's `ValidateBufferDataTarget` synthesizes.
fn buffer_for(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, target: u32) -> Option<u32> {
    let known = matches!(target, ARRAY_BUFFER | ELEMENT_ARRAY_BUFFER | 0x8F36 | 0x8F37 | 0x88EB | 0x88EC | TRANSFORM_FEEDBACK_BUFFER | UNIFORM_BUFFER);
    if !known {
        synth(scope, this, INVALID_ENUM);
        return None;
    }
    let b = edit(scope, this, |st| st.params.buffer_for(target));
    if b == 0 {
        synth(scope, this, INVALID_OPERATION);
        return None;
    }
    Some(b)
}

fn element_size(view: v8::Local<v8::ArrayBufferView>) -> i64 {
    let v: v8::Local<v8::Value> = view.into();
    if v.is_int16_array() || v.is_uint16_array() || v.is_float16_array() {
        2
    } else if v.is_int32_array() || v.is_uint32_array() || v.is_float32_array() {
        4
    } else if v.is_float64_array() || v.is_big_int64_array() || v.is_big_uint64_array() {
        8
    } else {
        1
    }
}

/// `endQuery`: the result becomes available only after the page has yielded to a task (Blink's
/// `WebGLQuery::ScheduleAllowAvailabilityUpdate`), so a query read in the same task answers `false`.
fn schedule_availability(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, q: u32) {
    let data = v8::Array::new_with_elements(scope, &[this.into(), v8::Integer::new_from_unsigned(scope, q).into()]);
    let Some(f) = v8::Function::builder(crate::binding::refs::f(cb_allow_availability)).data(data.into()).build(scope) else { return };
    let _ = crate::core::intersection_observer::rendering::pristine_timeout(scope, f, 0.0);
}

fn cb_allow_availability(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(d) = v8::Local::<v8::Array>::try_from(args.data()) else { return };
    let Some(ctx) = d.get_index(scope, 0).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
    let q = d.get_index(scope, 1).and_then(|v| v.uint32_value(scope)).unwrap_or(0);
    edit(scope, ctx, |st| {
        st.gl2.available.insert(q);
    });
}

// ── hooks the existing bodies call ───────────────────────────────────────────────────────────────

/// `bufferData` on the buffer bound to `target`: its contents become `data`, or `size` zero bytes.
pub(super) fn buffer_data(st: &mut super::WebGLState, target: u32, data: &[u8], size: usize) {
    let b = st.params.buffer_for(target);
    if b != 0 {
        st.gl2.mem.insert(b, if data.is_empty() { vec![0; size] } else { data.to_vec() });
    }
}

/// `bufferSubData`: `INVALID_VALUE` past the end (GL), else the bytes land.
pub(super) fn buffer_sub_data(st: &mut super::WebGLState, target: u32, offset: usize, data: &[u8]) -> Option<u32> {
    let b = st.params.buffer_for(target);
    let mem = st.gl2.mem.get_mut(&b)?;
    if offset + data.len() > mem.len() {
        return Some(INVALID_VALUE);
    }
    mem[offset..offset + data.len()].copy_from_slice(data);
    None
}

/// `texImage2D` on a texture `texStorage*` made immutable is `INVALID_OPERATION` (GL); otherwise the
/// level's size is recorded for `texSubImage2D`'s bounds.
pub(super) fn tex_image(st: &mut super::WebGLState, target: u32, level: i32, w: u32, h: u32) -> Option<u32> {
    let bind = if (0x8515..=0x851A).contains(&target) { TEXTURE_CUBE_MAP } else { target };
    let tex = st.params.texture_for(bind);
    if tex != 0 && st.gl2.immutable.contains_key(&tex) {
        return Some(INVALID_OPERATION);
    }
    if tex != 0 && target == TEXTURE_2D {
        st.gl2.levels.insert((tex, level), (w as i64, h as i64, 1));
    }
    None
}

/// `texSubImage2D`'s bounds against a level whose size is known: `INVALID_VALUE` outside it.
pub(super) fn tex_sub_image(st: &super::WebGLState, target: u32, level: i32, x: i64, y: i64, w: i64, h: i64) -> Option<u32> {
    if target != TEXTURE_2D {
        return None;
    }
    let tex = st.params.texture_for(TEXTURE_2D);
    let &(lw, lh, _) = st.gl2.levels.get(&(tex, level))?;
    (x < 0 || y < 0 || x + w > lw || y + h > lh).then_some(INVALID_VALUE)
}

/// `getTexParameter(TEXTURE_IMMUTABLE_FORMAT | TEXTURE_IMMUTABLE_LEVELS)`.
pub(super) fn immutable_param(st: &super::WebGLState, target: u32, pname: u32) -> Option<Result<bool, u32>> {
    let tex = st.params.texture_for(target);
    if tex == 0 {
        return None;
    }
    let imm = st.gl2.immutable.get(&tex);
    match pname {
        0x912F => Some(Ok(imm.is_some())),
        0x82DF => Some(Err(imm.map(|i| i.1).unwrap_or(0))),
        _ => None,
    }
}
