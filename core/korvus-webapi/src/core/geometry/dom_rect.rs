//! `DOMRectReadOnly` / `DOMRect` — the rectangle value-object family.

use deno_core::{CppgcBase, CppgcInherits};
use macros::webapi;

use super::{nan_safe_max, nan_safe_min};

// ---- DOMRectReadOnly / DOMRect: the first natively-generated Web API interfaces.
// Proves the native path end-to-end: cppgc getters/setters ([native code]), WebIDL inheritance
// (op2 `base`/`inherit` = C-struct embedding, DOMRect <: DOMRectReadOnly), a renamed method
// (toJSON), and a static (fromRect). Replaces the DOMRect JS glue in dom.js.

/// `toJSON()`'s object, in Blink's builder order (`dom_rect_read_only.cc:21-33`). A struct, not a
/// `serde_json::Value`: serde_json makes a NaN or an infinity `null` (`new DOMRect(NaN).toJSON().x` was `null`).
#[derive(serde::Serialize)]
pub(crate) struct DOMRectJson {
    x: f64, y: f64, width: f64, height: f64,
    top: f64, right: f64, bottom: f64, left: f64,
}

#[derive(serde::Deserialize, Default)]
pub(crate) struct DOMRectInit {
    // Fields are `pub(crate)` so `dom_quad::from_rect` (DOMQuad.fromRect) can read them across the
    // module split.
    #[serde(default)] pub(crate) x: f64,
    #[serde(default)] pub(crate) y: f64,
    #[serde(default)] pub(crate) width: f64,
    #[serde(default)] pub(crate) height: f64,
}

/// Native `DOMRectReadOnly` (base of `DOMRect`). Coordinates are `Cell`s so the mutable `DOMRect`
/// subclass — which inherits these getters — can write them through `&self`.
#[repr(C)]
#[derive(CppgcBase)]
pub struct DOMRectReadOnly {
    x: std::cell::Cell<f64>,
    y: std::cell::Cell<f64>,
    width: std::cell::Cell<f64>,
    height: std::cell::Cell<f64>,
}

impl DOMRectReadOnly {
    pub(crate) fn make(x: f64, y: f64, width: f64, height: f64) -> DOMRectReadOnly {
        DOMRectReadOnly {
            x: std::cell::Cell::new(x), y: std::cell::Cell::new(y),
            width: std::cell::Cell::new(width), height: std::cell::Cell::new(height),
        }
    }
}

// ---- The trait + the op2 glue below were EMITTED by a WebIDL scaffold generator that no longer exists
// (scripts/gen_webapi, then gen/src/emit.rs; both in git history) and are hand-maintained now. The
// glue below are machine-owned; regenerate, do not hand-edit. Behavior is the `impl
// DOMRectReadOnlyBehavior` block further down (human-owned). See docs/WEBAPI-ARCHITECTURE.md.
pub(crate) trait DOMRectReadOnlyBehavior {
    fn construct(_x: Option<f64>, _y: Option<f64>, _width: Option<f64>, _height: Option<f64>) -> DOMRectReadOnly where Self: Sized { stub!("DOMRectReadOnly::constructor") }
    fn from_rect(_other: Option<DOMRectInit>) -> DOMRectReadOnly where Self: Sized { stub!("DOMRectReadOnly::fromRect") }
    fn x(&self) -> f64 { stub!("DOMRectReadOnly::x") }
    fn y(&self) -> f64 { stub!("DOMRectReadOnly::y") }
    fn width(&self) -> f64 { stub!("DOMRectReadOnly::width") }
    fn height(&self) -> f64 { stub!("DOMRectReadOnly::height") }
    fn top(&self) -> f64 { stub!("DOMRectReadOnly::top") }
    fn right(&self) -> f64 { stub!("DOMRectReadOnly::right") }
    fn bottom(&self) -> f64 { stub!("DOMRectReadOnly::bottom") }
    fn left(&self) -> f64 { stub!("DOMRectReadOnly::left") }
    fn to_json(&self) -> DOMRectJson { stub!("DOMRectReadOnly::toJSON") }
}

#[webapi(base, own)]
impl DOMRectReadOnly {
    #[constructor]
    #[cppgc]
    fn constructor(x: Option<f64>, y: Option<f64>, width: Option<f64>, height: Option<f64>) -> Result<DOMRectReadOnly, ::deno_error::JsErrorBox> { dispatch!("DOMRectReadOnly::constructor", <DOMRectReadOnly as DOMRectReadOnlyBehavior>::construct(x, y, width, height)) }
    #[cppgc]
    #[reentrant]
    #[static_method]
    fn from_rect(#[serde] #[dict("DOMRectInit")] other: Option<DOMRectInit>) -> Result<DOMRectReadOnly, ::deno_error::JsErrorBox> { dispatch!("DOMRectReadOnly::fromRect", <DOMRectReadOnly as DOMRectReadOnlyBehavior>::from_rect(other)) }
    #[getter]
    fn x(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMRectReadOnly::x", <DOMRectReadOnly as DOMRectReadOnlyBehavior>::x(self)) }
    #[getter]
    fn y(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMRectReadOnly::y", <DOMRectReadOnly as DOMRectReadOnlyBehavior>::y(self)) }
    #[getter]
    fn width(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMRectReadOnly::width", <DOMRectReadOnly as DOMRectReadOnlyBehavior>::width(self)) }
    #[getter]
    fn height(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMRectReadOnly::height", <DOMRectReadOnly as DOMRectReadOnlyBehavior>::height(self)) }
    #[getter]
    fn top(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMRectReadOnly::top", <DOMRectReadOnly as DOMRectReadOnlyBehavior>::top(self)) }
    #[getter]
    fn right(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMRectReadOnly::right", <DOMRectReadOnly as DOMRectReadOnlyBehavior>::right(self)) }
    #[getter]
    fn bottom(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMRectReadOnly::bottom", <DOMRectReadOnly as DOMRectReadOnlyBehavior>::bottom(self)) }
    #[getter]
    fn left(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMRectReadOnly::left", <DOMRectReadOnly as DOMRectReadOnlyBehavior>::left(self)) }
    #[rename("toJSON")]
    #[object]
    fn to_json(&self) -> Result<DOMRectJson, ::deno_error::JsErrorBox> { dispatch!("DOMRectReadOnly::toJSON", <DOMRectReadOnly as DOMRectReadOnlyBehavior>::to_json(self)) }
}

// ---- BEHAVIOR (human-owned): the migrated hand-built bodies. The codegen never overwrites this.
impl DOMRectReadOnlyBehavior for DOMRectReadOnly {
    fn construct(x: Option<f64>, y: Option<f64>, width: Option<f64>, height: Option<f64>) -> DOMRectReadOnly {
        DOMRectReadOnly::make(x.unwrap_or(0.0), y.unwrap_or(0.0), width.unwrap_or(0.0), height.unwrap_or(0.0))
    }
    fn x(&self) -> f64 { self.x.get() }
    fn y(&self) -> f64 { self.y.get() }
    fn width(&self) -> f64 { self.width.get() }
    fn height(&self) -> f64 { self.height.get() }
    // Blink's `NanSafeMin` / `NanSafeMax` (`dom_rect_read_only.h:40-43`): a NaN coordinate or extent is a NaN edge
    fn top(&self) -> f64 { let (a, b) = (self.y.get(), self.y.get() + self.height.get()); nan_safe_min(a, b) }
    fn right(&self) -> f64 { let (a, b) = (self.x.get(), self.x.get() + self.width.get()); nan_safe_max(a, b) }
    fn bottom(&self) -> f64 { let (a, b) = (self.y.get(), self.y.get() + self.height.get()); nan_safe_max(a, b) }
    fn left(&self) -> f64 { let (a, b) = (self.x.get(), self.x.get() + self.width.get()); nan_safe_min(a, b) }
    fn from_rect(other: Option<DOMRectInit>) -> DOMRectReadOnly {
        let i = other.unwrap_or_default();
        DOMRectReadOnly::make(i.x, i.y, i.width, i.height)
    }
    fn to_json(&self) -> DOMRectJson {
        let (x, y, width, height) = (self.x.get(), self.y.get(), self.width.get(), self.height.get());
        DOMRectJson { x, y, width, height, top: self.top(), right: self.right(), bottom: self.bottom(), left: self.left() }
    }
}

/// Native `DOMRect`: `DOMRectReadOnly` plus writable x/y/width/height. cppgc inheritance requires
/// `#[repr(C)]` with the base as the first field.
#[repr(C)]
#[derive(CppgcInherits, CppgcBase)]
#[cppgc_inherits_from(DOMRectReadOnly)]
pub struct DOMRect {
    pub(crate) base: DOMRectReadOnly,
}

// ===== DOMRect (migrated to scaffold) =====
pub(crate) trait DOMRectBehavior {
   fn construct(_x: Option<f64>, _y: Option<f64>, _width: Option<f64>, _height: Option<f64>) -> DOMRect where Self: Sized { stub!("DOMRect::constructor") }
   fn x(&self) -> f64 { stub!("DOMRect::x") }
   fn set_x(&self, _v: f64)  { stub!("DOMRect::x=") }
   fn y(&self) -> f64 { stub!("DOMRect::y") }
   fn set_y(&self, _v: f64)  { stub!("DOMRect::y=") }
   fn width(&self) -> f64 { stub!("DOMRect::width") }
   fn set_width(&self, _v: f64)  { stub!("DOMRect::width=") }
   fn height(&self) -> f64 { stub!("DOMRect::height") }
   fn set_height(&self, _v: f64)  { stub!("DOMRect::height=") }
   fn from_rect(_init: Option<DOMRectInit>) -> DOMRect where Self: Sized { stub!("DOMRect::fromRect") }
}

#[webapi(inherit = DOMRectReadOnly, own)]
impl DOMRect {
    #[constructor]
    #[cppgc]
    fn constructor(x: Option<f64>, y: Option<f64>, width: Option<f64>, height: Option<f64>) -> Result<DOMRect, ::deno_error::JsErrorBox> { dispatch!("DOMRect::constructor", <DOMRect as DOMRectBehavior>::construct(x, y, width, height)) }
    #[getter]
    fn x(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMRect::x", <DOMRect as DOMRectBehavior>::x(self)) }
    #[setter]
    fn x(&self, v: f64) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("DOMRect::x=", <DOMRect as DOMRectBehavior>::set_x(self, v)) }
    #[getter]
    fn y(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMRect::y", <DOMRect as DOMRectBehavior>::y(self)) }
    #[setter]
    fn y(&self, v: f64) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("DOMRect::y=", <DOMRect as DOMRectBehavior>::set_y(self, v)) }
    #[getter]
    fn width(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMRect::width", <DOMRect as DOMRectBehavior>::width(self)) }
    #[setter]
    fn width(&self, v: f64) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("DOMRect::width=", <DOMRect as DOMRectBehavior>::set_width(self, v)) }
    #[getter]
    fn height(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMRect::height", <DOMRect as DOMRectBehavior>::height(self)) }
    #[setter]
    fn height(&self, v: f64) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("DOMRect::height=", <DOMRect as DOMRectBehavior>::set_height(self, v)) }
    #[cppgc]
    #[reentrant]
    #[static_method]
    fn from_rect(#[serde] #[dict("DOMRectInit")] init: Option<DOMRectInit>) -> Result<DOMRect, ::deno_error::JsErrorBox> { dispatch!("DOMRect::fromRect", <DOMRect as DOMRectBehavior>::from_rect(init)) }
}

impl DOMRectBehavior for DOMRect {
    fn construct(x: Option<f64>, y: Option<f64>, width: Option<f64>, height: Option<f64>) -> DOMRect { DOMRect { base: DOMRectReadOnly::make(x.unwrap_or(0.0), y.unwrap_or(0.0), width.unwrap_or(0.0), height.unwrap_or(0.0)) } }
    fn x(&self) -> f64 { self.base.x.get() }
    fn set_x(&self, v: f64)  { self.base.x.set(v) }
    fn y(&self) -> f64 { self.base.y.get() }
    fn set_y(&self, v: f64)  { self.base.y.set(v) }
    fn width(&self) -> f64 { self.base.width.get() }
    fn set_width(&self, v: f64)  { self.base.width.set(v) }
    fn height(&self) -> f64 { self.base.height.get() }
    fn set_height(&self, v: f64)  { self.base.height.set(v) }
    fn from_rect(init: Option<DOMRectInit>) -> DOMRect { let i = init.unwrap_or_default();
        DOMRect { base: DOMRectReadOnly::make(i.x, i.y, i.width, i.height) } }
}


// ── One-table definition (Phase 3 cutover, shape-first) ────────────────────────────────────────────
//
// The `interface!` tables for the DOMRect family. Bodies are `stub` for now: this locks the SURFACE
// SHAPE (members, order, descriptors, the DOMRect→DOMRectReadOnly chain, the `fromRect` static on the
// constructor) against the captured baseline via `install_constructable` + `describe_own`
// (tests/dom_rect_shape.rs), before the real cppgc constructor + `&self` getter bodies (the `impl
// …Behavior` blocks above, reached through the op2 `::DECL`) replace the stubs and the JS glue in
// 03-element-glue.js is removed. Value objects, exposed on window + worker.
webapi::interface! {
    "DOMRectReadOnly" [value, both, class DOMRectReadOnly] {
        ctor          "DOMRectReadOnly"           len 0 = stub,
        attr          "x"      : Double  readonly       = cppgc,
        attr          "y"      : Double  readonly       = cppgc,
        attr          "width"  : Double  readonly       = cppgc,
        attr          "height" : Double  readonly       = cppgc,
        attr          "top"    : Double  readonly       = cppgc,
        attr          "right"  : Double  readonly       = cppgc,
        attr          "bottom" : Double  readonly       = cppgc,
        attr          "left"   : Double  readonly       = cppgc,
        method        "toJSON" : Object          len 0  = cppgc,
        static_method "fromRect": Object         len 0  = cppgc,
    }
}

webapi::interface! {
    "DOMRect" : "DOMRectReadOnly" [value, both, class DOMRect] {
        ctor          "DOMRect"                len 0 = stub,
        attr          "x"      : Double              = cppgc,
        attr          "y"      : Double              = cppgc,
        attr          "width"  : Double              = cppgc,
        attr          "height" : Double              = cppgc,
        static_method "fromRect": Object       len 0 = cppgc,
    }
}

#[cfg(test)]
mod shape_tests {
    //! The DOMRect family's one-table SHAPE matches the captured Brave surface (Phase 3 §0.2):
    //! prototype members + order + descriptors, the fromRect static on the constructor, and the
    //! DOMRect -> DOMRectReadOnly prototype chain. Bodies are stub here (values are the live step).
    use super::{DOMRECT, DOMRECTREADONLY};
    use crate::install::installer::install_constructable;
    use crate::install::verify::describe_own;
    use deno_core::{v8, RuntimeOptions};

    #[test]
    fn dom_rect_family_surface_shape_matches_capture() {
        let mut rt = engine::new_runtime(RuntimeOptions::default());
        let ctx = rt.main_context();
        v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
        let global = scope.get_current_context().global(scope);

        // DOMRectReadOnly: 8 readonly accessors + toJSON, fromRect static, no parent.
        let rro_instance = ["x", "y", "width", "height", "top", "right", "bottom", "left", "toJSON"];
        let (rro_proto, rro_ctor, g1) =
            install_constructable(scope, global, &DOMRECTREADONLY, "DOMRectReadOnly", &rro_instance, &["fromRect"], None);
        assert!(g1.is_empty());
        // DOMRect: 4 writable accessors, fromRect static, parent DOMRectReadOnly.
        let (r_proto, r_ctor, g2) =
            install_constructable(scope, global, &DOMRECT, "DOMRect", &["x", "y", "width", "height"], &["fromRect"], Some((rro_proto, rro_ctor)));
        assert!(g2.is_empty());

        assert_eq!(
            describe_own(scope, rro_proto),
            vec![
                "x:accessor:e=1:c=1:s=0", "y:accessor:e=1:c=1:s=0", "width:accessor:e=1:c=1:s=0",
                "height:accessor:e=1:c=1:s=0", "top:accessor:e=1:c=1:s=0", "right:accessor:e=1:c=1:s=0",
                "bottom:accessor:e=1:c=1:s=0", "left:accessor:e=1:c=1:s=0", "toJSON:data:e=1:c=1:w=1",
                "constructor:data:e=0:c=1:w=1",
            ],
            "DOMRectReadOnly.prototype shape != capture",
        );
        assert_eq!(
            describe_own(scope, r_proto),
            vec![
                "x:accessor:e=1:c=1:s=1", "y:accessor:e=1:c=1:s=1", "width:accessor:e=1:c=1:s=1",
                "height:accessor:e=1:c=1:s=1", "constructor:data:e=0:c=1:w=1",
            ],
            "DOMRect.prototype shape != capture",
        );

        // fromRect is a static on the constructor, not the prototype. The capture records it
        // { enumerable:true, configurable:true, method } on BOTH ctors; describe_own on the ctor
        // (minus the three standard function own-props) must show exactly that.
        // Standard function own-props (length, name, prototype) are not part of the WebIDL static
        // surface; drop them and what remains must be exactly the captured statics.
        fn statics_only(all: Vec<String>) -> Vec<String> {
            all.into_iter()
                .filter(|l| !l.starts_with("length:") && !l.starts_with("name:") && !l.starts_with("prototype:"))
                .collect()
        }
        let rro_statics = statics_only(describe_own(scope, rro_ctor.into()));
        assert_eq!(rro_statics, vec!["fromRect:data:e=1:c=1:w=1"], "DOMRectReadOnly statics != capture");
        let r_statics = statics_only(describe_own(scope, r_ctor.into()));
        assert_eq!(r_statics, vec!["fromRect:data:e=1:c=1:w=1"], "DOMRect statics != capture");

        // The WebIDL inheritance splice: prototype chain AND constructor chain, plus toStringTag.
        let src = v8::String::new(
            scope,
            r#"[Object.getPrototypeOf(DOMRect.prototype) === DOMRectReadOnly.prototype,
                Object.getPrototypeOf(DOMRect) === DOMRectReadOnly,
                Object.prototype.toString.call(Object.create(DOMRect.prototype)),
                Object.prototype.toString.call(Object.create(DOMRectReadOnly.prototype))].join("|")"#,
        ).unwrap();
        let out = v8::Script::compile(scope, src, None).unwrap().run(scope).unwrap().to_rust_string_lossy(scope);
        assert_eq!(out, "true|true|[object DOMRect]|[object DOMRectReadOnly]");
    }
}
