//! `core/xr.rs` -- WebXR without a device: inline sessions, `XRRigidTransform`, `XRRay`
//! (the Chrome oracle ratchet).
//!
//! Every XR row was a generated stub, so `navigator.xr.requestSession("inline")` resolved `undefined`
//! where Chrome 150 -- with no headset -- resolves a real `XRSession`: WebXR's INLINE mode needs no
//! device (Blink: "inline sessions are always supported", `xr_system.cc`). What a page can read off one
//! is Blink's `XRSession` for an inline session with no features beyond "viewer":
//! `environmentBlendMode` "opaque", `interactionMode` "screen-space", `visibilityState` "visible" only
//! in a FOCUSED top-level document ("hidden" in a frame -- `XRSession::UpdateVisibilityState`), a render
//! state of depth 0.1 / 1000 and a pi/2 inline field of view, no input sources, the depth members
//! refusing with InvalidStateError, light probe / room capture / hit test refusing with
//! NotSupportedError ("Session does not support feature ..."), `maxRenderLayers` 1.
//!
//! `XRRigidTransform` and `XRRay` are pure math (`xr_rigid_transform.cc`, `xr_ray.cc`): their
//! validation messages, the normalized quaternion / direction, the lazily composed column-major
//! Float32Array matrix, the inverse.

use deno_core::v8;

use crate::binding::protos::{build_fn_data, get_prop, str_val, throw_dom, throw_type_error};
use crate::install::kits::define_accessor;
use crate::install::MemberDef;

fn slot_key<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, name).unwrap();
    v8::Private::for_api(scope, Some(n))
}
fn slot_get<'s>(scope: &mut v8::PinScope<'s, '_>, o: v8::Local<v8::Object>, name: &str) -> Option<v8::Local<'s, v8::Value>> {
    let k = slot_key(scope, name);
    o.get_private(scope, k).filter(|v| !v.is_undefined())
}
fn slot_set(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, name: &str, v: v8::Local<v8::Value>) {
    let k = slot_key(scope, name);
    o.set_private(scope, k, v);
}

/// A new instance of `iface` (its template's prototype), branded.
fn mint<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str) -> Option<v8::Local<'s, v8::Object>> {
    let ctor = crate::binding::protos::iface_object(scope, iface).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let proto = get_prop(scope, ctor, "prototype").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let o = crate::binding::protos::mint_on(scope, proto.into());
    crate::binding::protos::brand_shell_instance(scope, o);
    crate::install::webidl::brand(scope, o, iface);
    Some(o)
}

fn point<'s>(scope: &mut v8::PinScope<'s, '_>, x: f64, y: f64, z: f64, w: f64) -> v8::Local<'s, v8::Value> {
    let ctor = crate::binding::protos::iface_object(scope, "DOMPointReadOnly").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok());
    let args: Vec<v8::Local<v8::Value>> = [x, y, z, w].iter().map(|n| v8::Number::new(scope, *n).into()).collect();
    ctor.and_then(|c| c.new_instance(scope, &args)).map(|o| o.into()).unwrap_or_else(|| v8::null(scope).into())
}

fn f32_array<'s>(scope: &mut v8::PinScope<'s, '_>, m: &[f64; 16]) -> v8::Local<'s, v8::Value> {
    let bytes: Vec<u8> = m.iter().flat_map(|v| (*v as f32).to_le_bytes()).collect();
    let store = v8::ArrayBuffer::new_backing_store_from_vec(bytes).make_shared();
    let ab = v8::ArrayBuffer::with_backing_store(scope, &store);
    v8::Float32Array::new(scope, ab, 0, 16).map(|a| a.into()).unwrap_or_else(|| v8::null(scope).into())
}

fn nums(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, name: &str) -> Option<[f64; 4]> {
    let v = slot_get(scope, o, name)?;
    let a = v8::Local::<v8::Array>::try_from(v).ok()?;
    let mut out = [0.0; 4];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = a.get_index(scope, i as u32).and_then(|x| x.number_value(scope)).unwrap_or(0.0);
    }
    Some(out)
}
fn set_nums(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, name: &str, v: [f64; 4]) {
    let items: Vec<v8::Local<v8::Value>> = v.iter().map(|n| v8::Number::new(scope, *n).into()).collect();
    let a = v8::Array::new_with_elements(scope, &items);
    slot_set(scope, o, name, a.into());
}

/// A `DOMPointInit` dictionary (x, y, z default 0, w default 1) -- WebIDL `unrestricted double`, members
/// read in lexicographic order (w, x, y, z).
fn point_init(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, dflt: [f64; 4], ctx: &str, dict: &str) -> Result<[f64; 4], ()> {
    if v.is_null_or_undefined() {
        return Ok(dflt);
    }
    let Ok(o) = v8::Local::<v8::Object>::try_from(v) else {
        throw_type_error(scope, &format!("{ctx}The provided value is not of type '{dict}'."));
        return Err(());
    };
    let mut out = dflt;
    for (i, k) in [(3usize, "w"), (0, "x"), (1, "y"), (2, "z")] {
        let Some(x) = get_prop(scope, o, k) else { return Err(()) };
        if !x.is_undefined() {
            let Some(n) = x.number_value(scope) else { return Err(()) };
            out[i] = n;
        }
    }
    Ok(out)
}

// ── quaternion / matrix math (gfx::Transform, column-major) ───────────────────────────────────────

fn compose(p: [f64; 4], q: [f64; 4]) -> [f64; 16] {
    let (x, y, z, w) = (q[0], q[1], q[2], q[3]);
    [
        1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y + z * w), 2.0 * (x * z - y * w), 0.0,
        2.0 * (x * y - z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z + x * w), 0.0,
        2.0 * (x * z + y * w), 2.0 * (y * z - x * w), 1.0 - 2.0 * (x * x + y * y), 0.0,
        p[0], p[1], p[2], 1.0,
    ]
}

fn normalize4(q: [f64; 4]) -> [f64; 4] {
    let l = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    [q[0] / l, q[1] / l, q[2] / l, q[3] / l]
}

// ── XRRigidTransform ──────────────────────────────────────────────────────────────────────────────

/// `new XRRigidTransform(position, orientation)` -- Blink's `XRRigidTransform::Create`.
pub(crate) fn rigid_init(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, args: &v8::FunctionCallbackArguments) {
    let ctx = "Failed to construct 'XRRigidTransform': ";
    let has_p = !args.get(0).is_null_or_undefined();
    let has_o = !args.get(1).is_null_or_undefined();
    let Ok(p) = point_init(scope, args.get(0), [0.0, 0.0, 0.0, 1.0], ctx, "DOMPointInit") else { return };
    let Ok(o) = point_init(scope, args.get(1), [0.0, 0.0, 0.0, 1.0], ctx, "DOMPointInit") else { return };
    if has_p && p[3] != 1.0 {
        throw_type_error(scope, &format!("{ctx}W component of position must be 1.0"));
        return;
    }
    if (has_p && p.iter().any(|n| !n.is_finite())) || (has_o && o.iter().any(|n| !n.is_finite())) {
        throw_type_error(scope, &format!("{ctx}Position and Orientation must consist of only finite values"));
        return;
    }
    let mut q = [0.0, 0.0, 0.0, 1.0];
    if has_o {
        let sq = o.iter().map(|n| n * n).sum::<f64>();
        if sq == 0.0 {
            throw_dom(scope, &format!("{ctx}Orientation's length cannot be 0"), "InvalidStateError");
            return;
        }
        if !sq.is_finite() {
            throw_dom(scope, &format!("{ctx}Orientation is too large to normalize"), "InvalidStateError");
            return;
        }
        q = normalize4(o);
    }
    set_nums(scope, this, "xr_pos", [p[0], p[1], p[2], 1.0]);
    set_nums(scope, this, "xr_ori", q);
}

fn new_rigid<'s>(scope: &mut v8::PinScope<'s, '_>, p: [f64; 4], q: [f64; 4]) -> Option<v8::Local<'s, v8::Object>> {
    let o = mint(scope, "XRRigidTransform")?;
    set_nums(scope, o, "xr_pos", p);
    set_nums(scope, o, "xr_ori", q);
    Some(o)
}

fn cb_rigid_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let (Some(p), Some(q)) = (nums(scope, this, "xr_pos"), nums(scope, this, "xr_ori")) else {
        throw_type_error(scope, "Illegal invocation");
        return;
    };
    // [SameObject]: each member is made once and kept.
    let cache = format!("xr_c_{name}");
    if let Some(c) = slot_get(scope, this, &cache) {
        rv.set(c);
        return;
    }
    let v = match name.as_str() {
        "position" => point(scope, p[0], p[1], p[2], 1.0),
        "orientation" => point(scope, q[0], q[1], q[2], q[3]),
        "matrix" => {
            let m = compose(p, q);
            f32_array(scope, &m)
        }
        _ => {
            // The inverse of a rigid transform: the conjugate rotation, and the position rotated back
            // and negated.
            let iq = [-q[0], -q[1], -q[2], q[3]];
            let m = compose([0.0; 4], iq);
            let ip = [
                -(m[0] * p[0] + m[4] * p[1] + m[8] * p[2]),
                -(m[1] * p[0] + m[5] * p[1] + m[9] * p[2]),
                -(m[2] * p[0] + m[6] * p[1] + m[10] * p[2]),
                1.0,
            ];
            let ip = ip.map(|n| if n == 0.0 { 0.0 } else { n });
            match new_rigid(scope, ip, iq) {
                Some(o) => {
                    // The inverse's inverse is this transform.
                    slot_set(scope, o, "xr_c_inverse", this.into());
                    o.into()
                }
                None => return,
            }
        }
    };
    slot_set(scope, this, &cache, v);
    rv.set(v);
}

// ── XRRay ─────────────────────────────────────────────────────────────────────────────────────────

/// `new XRRay(origin, direction)` / `new XRRay(transform)` -- Blink's two constructors.
pub(crate) fn ray_init(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, args: &v8::FunctionCallbackArguments) {
    let ctx = "Failed to construct 'XRRay': ";
    let a0 = args.get(0);
    // The XRRigidTransform overload: the ray its matrix maps (0,0,0) -> (0,0,-1) to.
    if let Ok(t) = v8::Local::<v8::Object>::try_from(a0) {
        if let (Some(p), Some(q)) = (nums(scope, t, "xr_pos"), nums(scope, t, "xr_ori")) {
            let m = compose(p, q);
            let d = [-m[8], -m[9], -m[10]];
            let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            let dir = if l > 0.0 { [d[0] / l, d[1] / l, d[2] / l, 0.0] } else { [0.0, 0.0, -1.0, 0.0] };
            set_nums(scope, this, "xr_origin", [m[12], m[13], m[14], 1.0]);
            set_nums(scope, this, "xr_dir", dir);
            return;
        }
    }
    if !a0.is_null_or_undefined() && !a0.is_object() {
        throw_type_error(scope, &format!("{ctx}The provided value is not of type 'DOMPointInit'."));
        return;
    }
    let Ok(o) = point_init(scope, a0, [0.0, 0.0, 0.0, 1.0], ctx, "DOMPointInit") else { return };
    let Ok(d) = point_init(scope, args.get(1), [0.0, 0.0, -1.0, 0.0], ctx, "XRRayDirectionInit") else { return };
    let (dx, dy, dz) = (d[0] as f32, d[1] as f32, d[2] as f32);
    if dx * dx + dy * dy + dz * dz == 0.0 {
        throw_type_error(scope, &format!("{ctx}Unable to normalize vector of length 0."));
        return;
    }
    if d[3] as f32 != 0.0 {
        throw_type_error(scope, &format!("{ctx}Direction's `w` component must be set to 0.0f!"));
        return;
    }
    if o[3] as f32 != 1.0 {
        throw_type_error(scope, &format!("{ctx}Origin's `w` component must be set to 1.0f!"));
        return;
    }
    // gfx::Vector3dF::GetNormalized works in float.
    let l = (dx * dx + dy * dy + dz * dz).sqrt();
    let dir = [(dx / l) as f64, (dy / l) as f64, (dz / l) as f64, 0.0];
    set_nums(scope, this, "xr_origin", [o[0] as f32 as f64, o[1] as f32 as f64, o[2] as f32 as f64, 1.0]);
    set_nums(scope, this, "xr_dir", dir);
}

fn cb_ray_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    // A ray made with no arguments has its defaults.
    let o = nums(scope, this, "xr_origin").unwrap_or([0.0, 0.0, 0.0, 1.0]);
    let d = nums(scope, this, "xr_dir").unwrap_or([0.0, 0.0, -1.0, 0.0]);
    if !crate::install::webidl::has_brand(scope, this.into(), "XRRay") {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    let cache = format!("xr_c_{name}");
    if let Some(c) = slot_get(scope, this, &cache) {
        rv.set(c);
        return;
    }
    let v = match name.as_str() {
        "origin" => point(scope, o[0], o[1], o[2], 1.0),
        "direction" => point(scope, d[0], d[1], d[2], 0.0),
        _ => {
            // translation(origin) * rotation taking (0,0,-1) onto the direction (xr_ray.cc).
            let z = [0.0, 0.0, -1.0];
            let axis = [z[1] * d[2] - z[2] * d[1], z[2] * d[0] - z[0] * d[2], z[0] * d[1] - z[1] * d[0]];
            let cos = z[0] * d[0] + z[1] * d[1] + z[2] * d[2];
            let mut m = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, o[0], o[1], o[2], 1.0];
            if cos <= 0.9999 {
                let (ax, ay, az, ang) = if cos < -0.9999 {
                    (1.0, 0.0, 0.0, std::f64::consts::PI)
                } else {
                    let l = (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]).sqrt();
                    (axis[0] / l, axis[1] / l, axis[2] / l, cos.acos())
                };
                let (s, c) = ang.sin_cos();
                let t = 1.0 - c;
                let r = [
                    t * ax * ax + c, t * ax * ay + s * az, t * ax * az - s * ay,
                    t * ax * ay - s * az, t * ay * ay + c, t * ay * az + s * ax,
                    t * ax * az + s * ay, t * ay * az - s * ax, t * az * az + c,
                ];
                m[0] = r[0];
                m[1] = r[1];
                m[2] = r[2];
                m[4] = r[3];
                m[5] = r[4];
                m[6] = r[5];
                m[8] = r[6];
                m[9] = r[7];
                m[10] = r[8];
            }
            f32_array(scope, &m)
        }
    };
    slot_set(scope, this, &cache, v);
    rv.set(v);
}

// ── XRSystem / XRSession ──────────────────────────────────────────────────────────────────────────

const MODES: &[&str] = &["inline", "immersive-vr", "immersive-ar"];

fn reject_dom<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str, msg: &str) -> Option<v8::Local<'s, v8::Promise>> {
    let r = v8::PromiseResolver::new(scope)?;
    let e = crate::binding::protos::make_dom_exc(scope, msg, name)?;
    r.reject(scope, e);
    Some(r.get_promise(scope))
}

fn resolve_with<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<v8::Value>) -> Option<v8::Local<'s, v8::Promise>> {
    let r = v8::PromiseResolver::new(scope)?;
    r.resolve(scope, v);
    Some(r.get_promise(scope))
}

/// `navigator.xr.requestSession(mode, options)` / `isSessionSupported(mode)`.
fn cb_xr_system(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let method = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let mode = args.get(0).to_rust_string_lossy(scope);
    if !MODES.contains(&mode.as_str()) {
        let msg = format!("Failed to execute '{method}' on 'XRSystem': The provided value '{mode}' is not a valid enum value of type XRSessionMode.");
        if let Some(p) = v8::PromiseResolver::new(scope) {
            // Blink's context applied after the error is made
            let e = deno_core::error::native_error(scope, "TypeError", &msg);
            p.reject(scope, e);
            rv.set(p.get_promise(scope).into());
        }
        return;
    }
    if method == "isSessionSupported" {
        // Inline is always supported; immersive needs hardware this engine does not have.
        let b = v8::Boolean::new(scope, mode == "inline").into();
        if let Some(p) = resolve_with(scope, b) {
            rv.set(p.into());
        }
        return;
    }
    if mode != "inline" {
        if let Some(p) = reject_dom(scope, "NotSupportedError", "The specified session configuration is not supported.") {
            rv.set(p.into());
        }
        return;
    }
    // requiredFeatures beyond what an inline session has ("viewer") cannot be met.
    let required = v8::Local::<v8::Object>::try_from(args.get(1)).ok().and_then(|o| get_prop(scope, o, "requiredFeatures"));
    if let Some(req) = required.and_then(|r| v8::Local::<v8::Array>::try_from(r).ok()) {
        for i in 0..req.length() {
            let f = req.get_index(scope, i).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
            if f != "viewer" {
                if let Some(p) = reject_dom(scope, "NotSupportedError", "The specified session configuration is not supported.") {
                    rv.set(p.into());
                }
                return;
            }
        }
    }
    let Some(s) = mint(scope, "XRSession") else { return };
    if let Some(p) = resolve_with(scope, s.into()) {
        rv.set(p.into());
    }
}

/// Whether an inline session's frame is focused: the top-level document of a focused page.
fn session_visible(scope: &mut v8::PinScope) -> bool {
    crate::core::html::browsing_context::realm_document_focused(scope)
}

fn session_of(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> bool {
    if crate::install::webidl::has_brand(scope, this.into(), "XRSession") {
        return true;
    }
    throw_type_error(scope, "Illegal invocation");
    false
}

fn cb_session_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    if !session_of(scope, this) {
        return;
    }
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let depth_msg = "Depth sensing feature is not supported by the session.";
    match name.as_str() {
        "environmentBlendMode" => rv.set(str_val(scope, "opaque")),
        "interactionMode" => rv.set(str_val(scope, "screen-space")),
        "visibilityState" => {
            let v = if session_visible(scope) { "visible" } else { "hidden" };
            rv.set(str_val(scope, v))
        }
        "preferredReflectionFormat" => rv.set(str_val(scope, "rgba16f")),
        "domOverlayState" => rv.set_null(),
        "maxRenderLayers" => rv.set_uint32(1),
        "depthUsage" | "depthDataFormat" | "depthType" | "depthActive" => {
            throw_dom(scope, &format!("Failed to read the '{name}' property from 'XRSession': {depth_msg}"), "InvalidStateError");
        }
        "renderState" | "inputSources" | "enabledFeatures" => {
            let cache = format!("xr_c_{name}");
            if let Some(c) = slot_get(scope, this, &cache) {
                rv.set(c);
                return;
            }
            let v: v8::Local<v8::Value> = match name.as_str() {
                "renderState" => match mint(scope, "XRRenderState") {
                    Some(o) => o.into(),
                    None => return,
                },
                "inputSources" => match mint(scope, "XRInputSourceArray") {
                    Some(o) => o.into(),
                    None => return,
                },
                _ => {
                    let f = str_val(scope, "viewer");
                    let a = v8::Array::new_with_elements(scope, &[f]);
                    let _ = a.set_integrity_level(scope, v8::IntegrityLevel::Frozen);
                    a.into()
                }
            };
            slot_set(scope, this, &cache, v);
            rv.set(v);
        }
        _ => {}
    }
}

fn cb_session_method(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    // The promise-returning operations reject (never throw) for a foreign receiver.
    let promise_op = matches!(name.as_str(), "end" | "requestReferenceSpace" | "requestHitTestSource" | "requestHitTestSourceForTransientInput" | "requestLightProbe" | "initiateRoomCapture");
    if !crate::install::webidl::has_brand(scope, this.into(), "XRSession") {
        if promise_op {
            if let Some(p) = v8::PromiseResolver::new(scope) {
                let m = v8::String::new(scope, "Illegal invocation").unwrap();
                let e = v8::Exception::type_error(scope, m);
                p.reject(scope, e);
                rv.set(p.get_promise(scope).into());
            }
        } else {
            throw_type_error(scope, "Illegal invocation");
        }
        return;
    }
    let ended = slot_get(scope, this, "xr_ended").is_some();
    let session_msg = "XRSession has already ended.";
    match name.as_str() {
        "end" => {
            if ended {
                if let Some(p) = reject_dom(scope, "InvalidStateError", session_msg) {
                    rv.set(p.into());
                }
                return;
            }
            let t = v8::Boolean::new(scope, true).into();
            slot_set(scope, this, "xr_ended", t);
            let u = v8::undefined(scope).into();
            if let Some(p) = resolve_with(scope, u) {
                rv.set(p.into());
            }
            crate::core::html::browsing_context::fire_lifecycle(scope, this.into(), "end", false, None);
        }
        "requestReferenceSpace" => {
            let ty = args.get(0).to_rust_string_lossy(scope);
            // An inline session with no other feature has the "viewer" space only.
            if ty != "viewer" {
                if let Some(p) = reject_dom(scope, "NotSupportedError", "This device does not support the requested reference space type.") {
                    rv.set(p.into());
                }
                return;
            }
            let Some(s) = mint(scope, "XRReferenceSpace") else { return };
            if let Some(p) = resolve_with(scope, s.into()) {
                rv.set(p.into());
            }
        }
        "requestHitTestSource" | "requestHitTestSourceForTransientInput" | "requestLightProbe" | "initiateRoomCapture" => {
            // The options dictionary converts first: XRHitTestOptionsInit's `space` and
            // XRTransientInputHitTestOptionsInit's `profile` are required.
            let required = match name.as_str() {
                "requestHitTestSource" => Some(("space", "XRHitTestOptionsInit")),
                "requestHitTestSourceForTransientInput" => Some(("profile", "XRTransientInputHitTestOptionsInit")),
                _ => None,
            };
            if let Some((key, dict)) = required {
                let present = v8::Local::<v8::Object>::try_from(args.get(0)).ok().and_then(|o| get_prop(scope, o, key)).is_some_and(|v| !v.is_undefined());
                if !present {
                    let msg = if args.get(0).is_null_or_undefined() || args.get(0).is_object() {
                        format!("Failed to execute '{name}' on 'XRSession': Failed to read the '{key}' property from '{dict}': Required member is undefined.")
                    } else {
                        format!("Failed to execute '{name}' on 'XRSession': The provided value is not of type '{dict}'.")
                    };
                    if let Some(p) = v8::PromiseResolver::new(scope) {
                        // Blink's context applied after the error is made
                        let e = deno_core::error::native_error(scope, "TypeError", &msg);
                        p.reject(scope, e);
                        rv.set(p.get_promise(scope).into());
                    }
                    return;
                }
            }
            let feature = match name.as_str() {
                "requestLightProbe" => "light-estimation",
                "initiateRoomCapture" => "plane-detection",
                _ => "hit-test",
            };
            let msg = format!("Failed to execute '{name}' on 'XRSession': Session does not support feature {feature}");
            if let Some(p) = reject_dom(scope, "NotSupportedError", &msg) {
                rv.set(p.into());
            }
        }
        "pauseDepthSensing" | "resumeDepthSensing" => {
            let msg = if ended { session_msg } else { "Not called from inside an active animation frame." };
            throw_dom(scope, &format!("Failed to execute '{name}' on 'XRSession': {msg}"), "InvalidStateError");
        }
        "requestAnimationFrame" => {
            let n = slot_get(scope, this, "xr_raf").and_then(|v| v.number_value(scope)).unwrap_or(0.0) + 1.0;
            let v = v8::Number::new(scope, n).into();
            slot_set(scope, this, "xr_raf", v);
            rv.set_double(n);
        }
        // `updateRenderState(init)` with nothing an inline session refuses, and `cancelAnimationFrame`.
        _ => {}
    }
}

fn cb_render_state_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    if !crate::install::webidl::has_brand(scope, this.into(), "XRRenderState") {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    match name.as_str() {
        "depthNear" => rv.set_double(0.1),
        "depthFar" => rv.set_double(1000.0),
        "inlineVerticalFieldOfView" => rv.set_double(std::f64::consts::FRAC_PI_2),
        "baseLayer" => rv.set_null(),
        _ => {
            if let Some(c) = slot_get(scope, this, "xr_c_layers") {
                rv.set(c);
                return;
            }
            let a = v8::Array::new(scope, 0);
            let _ = a.set_integrity_level(scope, v8::IntegrityLevel::Frozen);
            slot_set(scope, this, "xr_c_layers", a.into());
            rv.set(a.into());
        }
    }
}

fn cb_input_sources_length(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    if !crate::install::webidl::has_brand(scope, this.into(), "XRInputSourceArray") {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    rv.set_uint32(0);
}

/// `XRWebGLLayer.getNativeFramebufferScaleFactor(session)`: an XRSession, and 0 for an inline one.
fn cb_native_scale(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let s = args.get(0);
    if !crate::install::webidl::has_brand(scope, s, "XRSession") {
        throw_type_error(scope, "Failed to execute 'getNativeFramebufferScaleFactor' on 'XRWebGLLayer': parameter 1 is not of type 'XRSession'.");
        return;
    }
    rv.set_double(0.0);
}

fn cb_offset_space(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    if !crate::install::webidl::has_brand(scope, this.into(), "XRReferenceSpace") {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    if !crate::install::webidl::has_brand(scope, args.get(0), "XRRigidTransform") {
        throw_type_error(scope, "Failed to execute 'getOffsetReferenceSpace' on 'XRReferenceSpace': parameter 1 is not of type 'XRRigidTransform'.");
        return;
    }
    if let Some(s) = mint(scope, "XRReferenceSpace") {
        rv.set(s.into());
    }
}

fn xr_getter(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef, cb: impl v8::MapFnTo<v8::FunctionCallback>) {
    let n = str_val(scope, member.name);
    let g = build_fn_data(scope, cb, &format!("get {}", member.name), 0, n);
    define_accessor(scope, proto, member, g, None);
}

fn xr_method(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef, cb: impl v8::MapFnTo<v8::FunctionCallback>) {
    let n = str_val(scope, member.name);
    crate::core::dom::shared::row_method_data(scope, proto, member, cb, n);
}

/// The table body of every XR row this file implements, dispatched on the row's `Interface::member`.
pub fn install_xr(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let iface = member.label.split("::").next().unwrap_or("");
    let attr = matches!(member.kind, crate::install::MemberKind::Attr { .. });
    match iface {
        "XRSystem" => xr_method(scope, proto, member, cb_xr_system),
        "XRSession" if attr => xr_getter(scope, proto, member, cb_session_get),
        "XRSession" => xr_method(scope, proto, member, cb_session_method),
        "XRRenderState" => xr_getter(scope, proto, member, cb_render_state_get),
        "XRInputSourceArray" => xr_getter(scope, proto, member, cb_input_sources_length),
        "XRRigidTransform" => xr_getter(scope, proto, member, cb_rigid_get),
        "XRRay" => xr_getter(scope, proto, member, cb_ray_get),
        "XRWebGLLayer" => xr_method(scope, proto, member, cb_native_scale),
        "XRReferenceSpace" => xr_method(scope, proto, member, cb_offset_space),
        _ => {}
    }
}
