//! `DOMPointReadOnly` / `DOMPoint` — the point value-object family.

use deno_core::{CppgcBase, CppgcInherits};
use macros::webapi;

use super::dom_matrix::MatrixLike;

// ---- DOMPointReadOnly / DOMPoint — the same value-object + inheritance pattern
// as DOMRect, now via #[webapi]. matrixTransform reads its DOMMatrix / 2D-init argument through
// #[serde] (a flexible matrix-like view), keeping DOMPoint fully native while DOMMatrix is still JS.

fn one() -> f64 { 1.0 }

/// `toJSON()`'s object, in Blink's builder order (`dom_point_read_only.cc:24-32`).
#[derive(serde::Serialize)]
pub(crate) struct DOMPointJson { x: f64, y: f64, z: f64, w: f64 }

#[derive(serde::Deserialize)]
pub(crate) struct PointInit {
    // Fields are `pub(crate)` so `dom_matrix::transform_point` can read them across the module split.
    #[serde(default)] pub(crate) x: f64,
    #[serde(default)] pub(crate) y: f64,
    #[serde(default)] pub(crate) z: f64,
    #[serde(default = "one")] pub(crate) w: f64,
}

// w defaults to 1 (both on a partial dictionary and on `fromPoint()` with no argument).
impl Default for PointInit {
    fn default() -> Self { PointInit { x: 0.0, y: 0.0, z: 0.0, w: 1.0 } }
}

#[repr(C)]
#[derive(CppgcBase)]
pub struct DOMPointReadOnly {
    x: std::cell::Cell<f64>,
    y: std::cell::Cell<f64>,
    z: std::cell::Cell<f64>,
    w: std::cell::Cell<f64>,
}

impl DOMPointReadOnly {
    pub(crate) fn make(x: f64, y: f64, z: f64, w: f64) -> DOMPointReadOnly {
        DOMPointReadOnly {
            x: std::cell::Cell::new(x), y: std::cell::Cell::new(y),
            z: std::cell::Cell::new(z), w: std::cell::Cell::new(w),
        }
    }
}

// ===== DOMPointReadOnly (migrated to scaffold) =====
pub(crate) trait DOMPointReadOnlyBehavior {
   fn construct(_x: Option<f64>, _y: Option<f64>, _z: Option<f64>, _w: Option<f64>) -> DOMPointReadOnly where Self: Sized { stub!("DOMPointReadOnly::constructor") }
   fn x(&self) -> f64 { stub!("DOMPointReadOnly::x") }
   fn y(&self) -> f64 { stub!("DOMPointReadOnly::y") }
   fn z(&self) -> f64 { stub!("DOMPointReadOnly::z") }
   fn w(&self) -> f64 { stub!("DOMPointReadOnly::w") }
   fn matrix_transform(&self, _mat: Option<MatrixLike>) -> Result<DOMPoint, ::deno_error::JsErrorBox> { stub!("DOMPointReadOnly::matrixTransform") }
   fn to_json(&self) -> DOMPointJson { stub!("DOMPointReadOnly::toJSON") }
   fn from_point(_init: Option<PointInit>) -> DOMPointReadOnly where Self: Sized { stub!("DOMPointReadOnly::fromPoint") }
}

#[webapi(base, own)]
impl DOMPointReadOnly {
    #[constructor]
    #[cppgc]
    fn constructor(x: Option<f64>, y: Option<f64>, z: Option<f64>, w: Option<f64>) -> Result<DOMPointReadOnly, ::deno_error::JsErrorBox> { dispatch!("DOMPointReadOnly::constructor", <DOMPointReadOnly as DOMPointReadOnlyBehavior>::construct(x, y, z, w)) }
    #[getter]
    fn x(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMPointReadOnly::x", <DOMPointReadOnly as DOMPointReadOnlyBehavior>::x(self)) }
    #[getter]
    fn y(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMPointReadOnly::y", <DOMPointReadOnly as DOMPointReadOnlyBehavior>::y(self)) }
    #[getter]
    fn z(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMPointReadOnly::z", <DOMPointReadOnly as DOMPointReadOnlyBehavior>::z(self)) }
    #[getter]
    fn w(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMPointReadOnly::w", <DOMPointReadOnly as DOMPointReadOnlyBehavior>::w(self)) }
    #[reentrant]
    #[cppgc]
    fn matrix_transform(&self, #[serde] #[dict("DOMMatrixInit")] mat: Option<MatrixLike>) -> Result<DOMPoint, ::deno_error::JsErrorBox> { dispatch_try!("DOMPointReadOnly::matrixTransform", <DOMPointReadOnly as DOMPointReadOnlyBehavior>::matrix_transform(self, mat)) }
    #[rename("toJSON")]
    #[object]
    fn to_json(&self) -> Result<DOMPointJson, ::deno_error::JsErrorBox> { dispatch!("DOMPointReadOnly::toJSON", <DOMPointReadOnly as DOMPointReadOnlyBehavior>::to_json(self)) }
    #[cppgc]
    #[reentrant]
    #[static_method]
    fn from_point(#[serde] #[dict("DOMPointInit")] init: Option<PointInit>) -> Result<DOMPointReadOnly, ::deno_error::JsErrorBox> { dispatch!("DOMPointReadOnly::fromPoint", <DOMPointReadOnly as DOMPointReadOnlyBehavior>::from_point(init)) }
}

impl DOMPointReadOnlyBehavior for DOMPointReadOnly {
    fn construct(x: Option<f64>, y: Option<f64>, z: Option<f64>, w: Option<f64>) -> DOMPointReadOnly { DOMPointReadOnly::make(x.unwrap_or(0.0), y.unwrap_or(0.0), z.unwrap_or(0.0), w.unwrap_or(1.0)) }
    fn x(&self) -> f64 { self.x.get() }
    fn y(&self) -> f64 { self.y.get() }
    fn z(&self) -> f64 { self.z.get() }
    fn w(&self) -> f64 { self.w.get() }
    // the matrix validated as `DOMMatrix.fromMatrix` validates it (Blink: `DOMMatrixReadOnly::fromMatrix(matrix)`)
    fn matrix_transform(&self, mat: Option<MatrixLike>) -> Result<DOMPoint, ::deno_error::JsErrorBox> {
        let (m, _) = mat.unwrap_or_default().fixup().map_err(|why| ::deno_error::JsErrorBox::type_error(format!("Failed to execute 'matrixTransform' on 'DOMPointReadOnly': {why}")))?;
        let (x, y, z, w) = (self.x.get(), self.y.get(), self.z.get(), self.w.get());
        Ok(DOMPoint { base: DOMPointReadOnly::make(
            m[0] * x + m[4] * y + m[8] * z + m[12] * w,
            m[1] * x + m[5] * y + m[9] * z + m[13] * w,
            m[2] * x + m[6] * y + m[10] * z + m[14] * w,
            m[3] * x + m[7] * y + m[11] * z + m[15] * w,
        ) }) }
    fn to_json(&self) -> DOMPointJson { DOMPointJson { x: self.x.get(), y: self.y.get(), z: self.z.get(), w: self.w.get() } }
    fn from_point(init: Option<PointInit>) -> DOMPointReadOnly { let i = init.unwrap_or_default();
        DOMPointReadOnly::make(i.x, i.y, i.z, i.w) }
}

#[repr(C)]
#[derive(CppgcInherits, CppgcBase)]
#[cppgc_inherits_from(DOMPointReadOnly)]
pub struct DOMPoint {
    pub(crate) base: DOMPointReadOnly,
}

// ===== DOMPoint (migrated to scaffold) =====
pub(crate) trait DOMPointBehavior {
   fn construct(_x: Option<f64>, _y: Option<f64>, _z: Option<f64>, _w: Option<f64>) -> DOMPoint where Self: Sized { stub!("DOMPoint::constructor") }
   fn x(&self) -> f64 { stub!("DOMPoint::x") }
   fn set_x(&self, _v: f64)  { stub!("DOMPoint::x=") }
   fn y(&self) -> f64 { stub!("DOMPoint::y") }
   fn set_y(&self, _v: f64)  { stub!("DOMPoint::y=") }
   fn z(&self) -> f64 { stub!("DOMPoint::z") }
   fn set_z(&self, _v: f64)  { stub!("DOMPoint::z=") }
   fn w(&self) -> f64 { stub!("DOMPoint::w") }
   fn set_w(&self, _v: f64)  { stub!("DOMPoint::w=") }
   fn from_point(_init: Option<PointInit>) -> DOMPoint where Self: Sized { stub!("DOMPoint::fromPoint") }
}

#[webapi(inherit = DOMPointReadOnly, own)]
impl DOMPoint {
    #[constructor]
    #[cppgc]
    fn constructor(x: Option<f64>, y: Option<f64>, z: Option<f64>, w: Option<f64>) -> Result<DOMPoint, ::deno_error::JsErrorBox> { dispatch!("DOMPoint::constructor", <DOMPoint as DOMPointBehavior>::construct(x, y, z, w)) }
    #[getter]
    fn x(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMPoint::x", <DOMPoint as DOMPointBehavior>::x(self)) }
    #[setter]
    fn x(&self, v: f64) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("DOMPoint::x=", <DOMPoint as DOMPointBehavior>::set_x(self, v)) }
    #[getter]
    fn y(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMPoint::y", <DOMPoint as DOMPointBehavior>::y(self)) }
    #[setter]
    fn y(&self, v: f64) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("DOMPoint::y=", <DOMPoint as DOMPointBehavior>::set_y(self, v)) }
    #[getter]
    fn z(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMPoint::z", <DOMPoint as DOMPointBehavior>::z(self)) }
    #[setter]
    fn z(&self, v: f64) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("DOMPoint::z=", <DOMPoint as DOMPointBehavior>::set_z(self, v)) }
    #[getter]
    fn w(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMPoint::w", <DOMPoint as DOMPointBehavior>::w(self)) }
    #[setter]
    fn w(&self, v: f64) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("DOMPoint::w=", <DOMPoint as DOMPointBehavior>::set_w(self, v)) }
    #[cppgc]
    #[reentrant]
    #[static_method]
    fn from_point(#[serde] #[dict("DOMPointInit")] init: Option<PointInit>) -> Result<DOMPoint, ::deno_error::JsErrorBox> { dispatch!("DOMPoint::fromPoint", <DOMPoint as DOMPointBehavior>::from_point(init)) }
}

impl DOMPointBehavior for DOMPoint {
    fn construct(x: Option<f64>, y: Option<f64>, z: Option<f64>, w: Option<f64>) -> DOMPoint { DOMPoint { base: DOMPointReadOnly::make(x.unwrap_or(0.0), y.unwrap_or(0.0), z.unwrap_or(0.0), w.unwrap_or(1.0)) } }
    fn x(&self) -> f64 { self.base.x.get() }
    fn set_x(&self, v: f64)  { self.base.x.set(v) }
    fn y(&self) -> f64 { self.base.y.get() }
    fn set_y(&self, v: f64)  { self.base.y.set(v) }
    fn z(&self) -> f64 { self.base.z.get() }
    fn set_z(&self, v: f64)  { self.base.z.set(v) }
    fn w(&self) -> f64 { self.base.w.get() }
    fn set_w(&self, v: f64)  { self.base.w.set(v) }
    fn from_point(init: Option<PointInit>) -> DOMPoint { let i = init.unwrap_or_default();
        DOMPoint { base: DOMPointReadOnly::make(i.x, i.y, i.z, i.w) } }
}


// ── One-table definition (Phase 5 F1a cutover, shape-first) ─────────────────────────────────────────
//
// The `interface!` tables for the DOMPoint family, mirroring the proven DOMRect family (dom_rect.rs).
// Bodies are `stub`: the SHAPE — members, captured order (x,y,z,w,matrixTransform,toJSON), descriptors,
// the DOMPoint→DOMPointReadOnly chain, and the `fromPoint` static on each constructor — is locked
// against the captured Brave surface here (`shape_tests` below). The real cppgc bodies (the `impl
// …Behavior` blocks above, reached through the op2 `::DECL`) supply values live via
// `install::live::reconcile_dom_point_family`, which becomes the shape authority once DOMPoint is
// removed from the shell registry (the generators skip every `interface!` name: scripts/table_inventory.py).
// Value objects, exposed on window + worker.
webapi::interface! {
    "DOMPointReadOnly" [value, both, class DOMPointReadOnly] {
        ctor          "DOMPointReadOnly"                len 0 = stub,
        attr          "x"      : Double  readonly             = cppgc,
        attr          "y"      : Double  readonly             = cppgc,
        attr          "z"      : Double  readonly             = cppgc,
        attr          "w"      : Double  readonly             = cppgc,
        method        "matrixTransform" : Object       len 0  = cppgc,
        method        "toJSON"          : Object       len 0  = cppgc,
        static_method "fromPoint"       : Object       len 0  = cppgc,
    }
}

webapi::interface! {
    "DOMPoint" : "DOMPointReadOnly" [value, both, class DOMPoint] {
        ctor          "DOMPoint"                    len 0 = stub,
        attr          "x"      : Double                   = cppgc,
        attr          "y"      : Double                   = cppgc,
        attr          "z"      : Double                   = cppgc,
        attr          "w"      : Double                   = cppgc,
        static_method "fromPoint"          : Object  len 0 = cppgc,
    }
}

#[cfg(test)]
mod shape_tests {
    //! The DOMPoint family's one-table SHAPE matches the captured Brave surface (Phase 5 F1a):
    //! prototype members + order + descriptors, the `fromPoint` static on the constructor, and the
    //! DOMPoint -> DOMPointReadOnly prototype chain. Bodies are stub here (values are live via
    //! `install::live::reconcile_dom_point_family`). Mirrors `dom_rect.rs`'s `shape_tests`.
    use super::{DOMPOINT, DOMPOINTREADONLY};
    use crate::install::installer::install_constructable;
    use crate::install::verify::describe_own;
    use deno_core::{v8, RuntimeOptions};

    #[test]
    fn dom_point_family_surface_shape_matches_capture() {
        let mut rt = engine::new_runtime(RuntimeOptions::default());
        let ctx = rt.main_context();
        v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
        let global = scope.get_current_context().global(scope);

        // DOMPointReadOnly: 4 readonly accessors + matrixTransform + toJSON, fromPoint static, no parent.
        let ro_instance = ["x", "y", "z", "w", "matrixTransform", "toJSON"];
        let (ro_proto, ro_ctor, g1) =
            install_constructable(scope, global, &DOMPOINTREADONLY, "DOMPointReadOnly", &ro_instance, &["fromPoint"], None);
        assert!(g1.is_empty());
        // DOMPoint: 4 writable accessors, fromPoint static, parent DOMPointReadOnly.
        let (p_proto, p_ctor, g2) =
            install_constructable(scope, global, &DOMPOINT, "DOMPoint", &["x", "y", "z", "w"], &["fromPoint"], Some((ro_proto, ro_ctor)));
        assert!(g2.is_empty());

        assert_eq!(
            describe_own(scope, ro_proto),
            vec![
                "x:accessor:e=1:c=1:s=0", "y:accessor:e=1:c=1:s=0", "z:accessor:e=1:c=1:s=0",
                "w:accessor:e=1:c=1:s=0", "matrixTransform:data:e=1:c=1:w=1", "toJSON:data:e=1:c=1:w=1",
                "constructor:data:e=0:c=1:w=1",
            ],
            "DOMPointReadOnly.prototype shape != capture",
        );
        assert_eq!(
            describe_own(scope, p_proto),
            vec![
                "x:accessor:e=1:c=1:s=1", "y:accessor:e=1:c=1:s=1", "z:accessor:e=1:c=1:s=1",
                "w:accessor:e=1:c=1:s=1", "constructor:data:e=0:c=1:w=1",
            ],
            "DOMPoint.prototype shape != capture",
        );

        // fromPoint is a static on each constructor, not the prototype. Drop the standard function
        // own-props; what remains must be exactly the captured statics.
        fn statics_only(all: Vec<String>) -> Vec<String> {
            all.into_iter()
                .filter(|l| !l.starts_with("length:") && !l.starts_with("name:") && !l.starts_with("prototype:"))
                .collect()
        }
        let ro_statics = statics_only(describe_own(scope, ro_ctor.into()));
        assert_eq!(ro_statics, vec!["fromPoint:data:e=1:c=1:w=1"], "DOMPointReadOnly statics != capture");
        let p_statics = statics_only(describe_own(scope, p_ctor.into()));
        assert_eq!(p_statics, vec!["fromPoint:data:e=1:c=1:w=1"], "DOMPoint statics != capture");

        // The WebIDL inheritance splice: prototype chain AND constructor chain, plus toStringTag.
        let src = v8::String::new(
            scope,
            r#"[Object.getPrototypeOf(DOMPoint.prototype) === DOMPointReadOnly.prototype,
                Object.getPrototypeOf(DOMPoint) === DOMPointReadOnly,
                Object.prototype.toString.call(Object.create(DOMPoint.prototype)),
                Object.prototype.toString.call(Object.create(DOMPointReadOnly.prototype))].join("|")"#,
        ).unwrap();
        let out = v8::Script::compile(scope, src, None).unwrap().run(scope).unwrap().to_rust_string_lossy(scope);
        assert_eq!(out, "true|true|[object DOMPoint]|[object DOMPointReadOnly]");
    }
}
