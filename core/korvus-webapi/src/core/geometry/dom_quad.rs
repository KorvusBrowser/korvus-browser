//! `DOMQuad` — the quadrilateral value-object (geometry family).
//!
//! The same value-object pattern as `dom_rect`/`dom_point`: the machine-generated `DOMQuadBehavior`
//! trait + `#[webapi]` op2 glue (mirrors `gen/tests/fixtures/emit/DOMQuad.txt`), with the
//! human-owned `impl DOMQuadBehavior` bodies implementing the geometry-1 spec:
//!   - the constructor reads its four `DOMPointInit` dictionaries (each defaulting to `{x:0, y:0,
//!     z:0, w:1}` — w defaults to 1, like `DOMPoint`),
//!   - `fromRect` pins the quad to the rect's corners (p1=(x,y), p2=(x+w,y), p3=(x+w,y+h),
//!     p4=(x,y+h), each z=0 w=1), and `fromQuad` copies `DOMQuadInit`'s points,
//!   - `getBounds` returns the tight `DOMRect` (left/top = min, width/height = max−min), and
//!   - `toJSON` serializes as `DOMQuadInit` ({p1:{x,y,z,w}, …, p4:{…}}).
//!
//! Replaces the generated_shells.js DOMQuad stub (whose ctor threw "Illegal constructor"), so
//! `new DOMQuad()` constructs per WebIDL — the constructability gap the quality report flagged.

use deno_core::{v8, CppgcBase};
use macros::webapi;

use super::dom_point::{DOMPoint, DOMPointReadOnly, PointInit as DOMPointInit};
use super::dom_rect::{DOMRect, DOMRectInit, DOMRectReadOnly};
use super::{nan_safe_max, nan_safe_min};

#[derive(serde::Deserialize, Default)]
pub(crate) struct DOMQuadInit {
    #[serde(default)] p1: Option<DOMPointInit>,
    #[serde(default)] p2: Option<DOMPointInit>,
    #[serde(default)] p3: Option<DOMPointInit>,
    #[serde(default)] p4: Option<DOMPointInit>,
}

/// One stored corner. `Copy` — every access hands out a fresh `DOMPoint` value, the same
/// fresh-wrapper-per-access shape `DOMPointReadOnly::matrixTransform` already established.
#[derive(Clone, Copy)]
struct QuadPoint { x: f64, y: f64, z: f64, w: f64 }

impl QuadPoint {
    fn from_init(i: DOMPointInit) -> QuadPoint { QuadPoint { x: i.x, y: i.y, z: i.z, w: i.w } }
    fn point(self) -> DOMPoint { DOMPoint { base: DOMPointReadOnly::make(self.x, self.y, self.z, self.w) } }
    fn json(self) -> QuadPointJson { QuadPointJson { x: self.x, y: self.y, z: self.z, w: self.w } }
}

impl Default for QuadPoint {
    // A missing DOMQuadInit corner is a default DOMPointInit: (0, 0, 0, 1).
    fn default() -> Self { QuadPoint { x: 0.0, y: 0.0, z: 0.0, w: 1.0 } }
}

#[derive(serde::Serialize)]
struct QuadPointJson { x: f64, y: f64, z: f64, w: f64 }

#[derive(serde::Serialize)]
struct DOMQuadJson { p1: QuadPointJson, p2: QuadPointJson, p3: QuadPointJson, p4: QuadPointJson }

/// Native `DOMQuad`. Corners are plain immutable data — every attribute is readonly.
#[repr(C)]
#[derive(CppgcBase)]
pub struct DOMQuad {
    p1: QuadPoint,
    p2: QuadPoint,
    p3: QuadPoint,
    p4: QuadPoint,
}

impl DOMQuad {
    fn make(p1: QuadPoint, p2: QuadPoint, p3: QuadPoint, p4: QuadPoint) -> DOMQuad {
        DOMQuad { p1, p2, p3, p4 }
    }
}

// ===== DOMQuad (migrated to scaffold) =====
pub(crate) trait DOMQuadBehavior {
    fn construct(_p1: Option<DOMPointInit>, _p2: Option<DOMPointInit>, _p3: Option<DOMPointInit>, _p4: Option<DOMPointInit>) -> DOMQuad where Self: Sized { stub!("DOMQuad::constructor") }
    fn from_rect(_other: Option<DOMRectInit>) -> DOMQuad where Self: Sized { stub!("DOMQuad::fromRect") }
    fn from_quad(_other: Option<DOMQuadInit>) -> DOMQuad where Self: Sized { stub!("DOMQuad::fromQuad") }
    fn p1(&self) -> DOMPoint { stub!("DOMQuad::p1") }
    fn p2(&self) -> DOMPoint { stub!("DOMQuad::p2") }
    fn p3(&self) -> DOMPoint { stub!("DOMQuad::p3") }
    fn p4(&self) -> DOMPoint { stub!("DOMQuad::p4") }
    fn get_bounds(&self) -> DOMRect { stub!("DOMQuad::getBounds") }
    fn to_json(&self) -> serde_json::Value { stub!("DOMQuad::toJSON") }
}

#[webapi(base, own)]
impl DOMQuad {
    #[constructor]
    #[cppgc]
    #[reentrant]
    fn constructor(#[serde] #[dict("DOMPointInit")] p1: Option<DOMPointInit>, #[serde] #[dict("DOMPointInit")] p2: Option<DOMPointInit>, #[serde] #[dict("DOMPointInit")] p3: Option<DOMPointInit>, #[serde] #[dict("DOMPointInit")] p4: Option<DOMPointInit>) -> Result<DOMQuad, ::deno_error::JsErrorBox> { dispatch!("DOMQuad::constructor", <DOMQuad as DOMQuadBehavior>::construct(p1, p2, p3, p4)) }
    #[cppgc]
    #[reentrant]
    #[static_method]
    fn from_rect(#[serde] #[dict("DOMRectInit")] other: Option<DOMRectInit>) -> Result<DOMQuad, ::deno_error::JsErrorBox> { dispatch!("DOMQuad::fromRect", <DOMQuad as DOMQuadBehavior>::from_rect(other)) }
    #[cppgc]
    #[reentrant]
    #[static_method]
    fn from_quad(#[serde] #[dict("DOMQuadInit")] other: Option<DOMQuadInit>) -> Result<DOMQuad, ::deno_error::JsErrorBox> { dispatch!("DOMQuad::fromQuad", <DOMQuad as DOMQuadBehavior>::from_quad(other)) }
    #[getter]
    #[cppgc]
    fn p1(&self) -> Result<DOMPoint, ::deno_error::JsErrorBox> { dispatch!("DOMQuad::p1", <DOMQuad as DOMQuadBehavior>::p1(self)) }
    #[getter]
    #[cppgc]
    fn p2(&self) -> Result<DOMPoint, ::deno_error::JsErrorBox> { dispatch!("DOMQuad::p2", <DOMQuad as DOMQuadBehavior>::p2(self)) }
    #[getter]
    #[cppgc]
    fn p3(&self) -> Result<DOMPoint, ::deno_error::JsErrorBox> { dispatch!("DOMQuad::p3", <DOMQuad as DOMQuadBehavior>::p3(self)) }
    #[getter]
    #[cppgc]
    fn p4(&self) -> Result<DOMPoint, ::deno_error::JsErrorBox> { dispatch!("DOMQuad::p4", <DOMQuad as DOMQuadBehavior>::p4(self)) }
    #[cppgc]
    fn get_bounds(&self) -> Result<DOMRect, ::deno_error::JsErrorBox> { dispatch!("DOMQuad::getBounds", <DOMQuad as DOMQuadBehavior>::get_bounds(self)) }
    #[object]
    #[rename("toJSON")]
    fn to_json(&self) -> Result<serde_json::Value, ::deno_error::JsErrorBox> { dispatch!("DOMQuad::toJSON", <DOMQuad as DOMQuadBehavior>::to_json(self)) }
}

impl DOMQuadBehavior for DOMQuad {
    fn construct(p1: Option<DOMPointInit>, p2: Option<DOMPointInit>, p3: Option<DOMPointInit>, p4: Option<DOMPointInit>) -> DOMQuad {
        let pt = |i: Option<DOMPointInit>| QuadPoint::from_init(i.unwrap_or_default());
        DOMQuad::make(pt(p1), pt(p2), pt(p3), pt(p4))
    }
    fn from_rect(other: Option<DOMRectInit>) -> DOMQuad {
        let i = other.unwrap_or_default();
        DOMQuad::make(
            QuadPoint { x: i.x, y: i.y, z: 0.0, w: 1.0 },
            QuadPoint { x: i.x + i.width, y: i.y, z: 0.0, w: 1.0 },
            QuadPoint { x: i.x + i.width, y: i.y + i.height, z: 0.0, w: 1.0 },
            QuadPoint { x: i.x, y: i.y + i.height, z: 0.0, w: 1.0 },
        )
    }
    fn from_quad(other: Option<DOMQuadInit>) -> DOMQuad {
        let i = other.unwrap_or_default();
        DOMQuad::make(
            i.p1.map(QuadPoint::from_init).unwrap_or_default(),
            i.p2.map(QuadPoint::from_init).unwrap_or_default(),
            i.p3.map(QuadPoint::from_init).unwrap_or_default(),
            i.p4.map(QuadPoint::from_init).unwrap_or_default(),
        )
    }
    fn p1(&self) -> DOMPoint { self.p1.point() }
    fn p2(&self) -> DOMPoint { self.p2.point() }
    fn p3(&self) -> DOMPoint { self.p3.point() }
    fn p4(&self) -> DOMPoint { self.p4.point() }
    // Blink's `DOMQuad::getBounds` (`dom_quad.cc:57-101`): `NanSafeMin4` / `NanSafeMax4` -- one NaN corner makes a NaN
    // bound, where a fold over `f64::min` skipped it (`new DOMQuad({x: NaN}).getBounds().x` was 0).
    fn get_bounds(&self) -> DOMRect {
        let min4 = |a: f64, b: f64, c: f64, d: f64| nan_safe_min(nan_safe_min(a, b), nan_safe_min(c, d));
        let max4 = |a: f64, b: f64, c: f64, d: f64| nan_safe_max(nan_safe_max(a, b), nan_safe_max(c, d));
        let (p1, p2, p3, p4) = (self.p1, self.p2, self.p3, self.p4);
        let (left, top) = (min4(p1.x, p2.x, p3.x, p4.x), min4(p1.y, p2.y, p3.y, p4.y));
        let (right, bottom) = (max4(p1.x, p2.x, p3.x, p4.x), max4(p1.y, p2.y, p3.y, p4.y));
        DOMRect { base: DOMRectReadOnly::make(left, top, right - left, bottom - top) }
    }
    fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(DOMQuadJson { p1: self.p1.json(), p2: self.p2.json(), p3: self.p3.json(), p4: self.p4.json() }).unwrap()
    }
}

/// `DOMQuad.prototype.toJSON()` as a raw callback, because the answer is a shape op2 cannot render.
///
/// Chrome returns a plain object whose four values are REAL `DOMPoint` instances: `toString` says
/// `[object DOMPoint]`, `instanceof DOMPoint` holds, and `Object.keys(p1)` is EMPTY because x/y/z/w
/// live on the prototype. The op2 form returned `serde_json::Value`, which renders as plain nested
/// objects -- so `Object.keys(q.toJSON().p1)` answered "x,y,z,w" where Chrome answers "". A page that
/// walks the result sees a different object graph.
///
/// It is Blink's `DOMQuad::toJSONForBinding` (`dom_quad.cc:120-127`): a `V8ObjectBuilder` -- an ordinary object of
/// the current realm, each point DEFINED on it (CreateDataProperty) -- over the quad's OWN points. It read them
/// through `this.p1` & co. and wrote them with `[[Set]]` (measured on Chrome 150): a page's getter on
/// `DOMQuad.prototype.p1` answered for the quad (`toJSON().p1` was the page's string), a page's setter on
/// `Object.prototype.p1` swallowed the point (`Object.keys(q.toJSON())` was "p2,p3,p4"). Each point is minted as the
/// `p1` getter mints it (a new `DOMPoint` per read -- Chrome's quad holds four, the same objects every time).
fn cb_quad_to_json(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(quad) = deno_core::_ops::try_unwrap_cppgc_base_object::<DOMQuad>(scope, args.this().into()) else {
        crate::binding::class::illegal_invocation(scope);
        return;
    };
    let points = {
        let quad = unsafe { quad.as_ref() };
        [quad.p1, quad.p2, quad.p3, quad.p4]
    };
    let out = v8::Object::new(scope);
    for (name, p) in ["p1", "p2", "p3", "p4"].into_iter().zip(points) {
        let Some(key) = v8::String::new(scope, name) else { return };
        let point = crate::binding::class::mint(scope, p.point());
        if out.create_data_property(scope, key.into(), point.into()) != Some(true) {
            return;
        }
    }
    rv.set(out.into());
}
/// The one row of [`DOMQUAD`] whose body is this file's rather than the class template's.
///
/// `toJSON` IS generated from the `impl` above -- and renders WRONG, which is why the row is an
/// `Install` and not a `cppgc`: the table names this body, in its sixth position. (Once
/// the op2 class template realised the wrong one and `installer::verify_native`, deleted then,
/// mounted this one over it.) Earlier
/// this was an ESM `Object.defineProperty` behind an op (`op_quad_to_json`, deleted with it).
pub fn install_dom_quad(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static crate::install::MemberDef) {
    match member.name {
        "toJSON" => crate::install::live::rows::method_row(scope, proto, member, cb_quad_to_json),
        other => debug_assert!(false, "install_dom_quad: unexpected member {other}"),
    }
}


// ── One-table definition ─────────────────────────────────────────────────────────
//
// Rows VERBATIM from the `DOMQuad` block of `install/generated_registry.rs`, which is the capture's
// order and webref's types/lengths/required counts. `deno_core` realises the whole interface from the
// `#[op2] impl` above, in the SAME order (p1..p4, getBounds, toJSON), so
// `install::live::reconcile_dom_quad` VERIFIES the rows in place rather than minting a prototype --
// the `Symbol.toStringTag`, the WebIDL `.length`s and the `toJSON` body are what it supplies.
//
// `[value, both]`: `DOMQuad` is `[Exposed=(Window,Worker)]` and the worker global takes the same
// native class (`js/worker-host.js`), so the same table serves both realms and there is no second
// shell set (goal invariant I8).
webapi::interface! {
    "DOMQuad" [value, both, class DOMQuad] {
        ctor          "DOMQuad"               len 0 = stub,
        attr          "p1"        : Any readonly     = cppgc,
        attr          "p2"        : Any readonly     = cppgc,
        attr          "p3"        : Any readonly     = cppgc,
        attr          "p4"        : Any readonly     = cppgc,
        method        "getBounds" : Any       len 0  = cppgc,
        method        "toJSON"    : Any       len 0  = install_dom_quad,
        static_method "fromQuad"  : Any       len 0  = cppgc,
        static_method "fromRect"  : Any       len 0  = cppgc
    }
}
