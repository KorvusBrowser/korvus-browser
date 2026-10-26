//! Geometry Web-API interfaces: the `DOMRect` / `DOMPoint` / `DOMMatrix` families.
//!
//! One file per interface-family (the blueprint's Chromium-style `core/<domain>/` tree). Each
//! interface keeps its layers together: the value/init structs, the machine-generated `…Behavior`
//! trait + `#[webapi]` op2 glue (regenerate, do not hand-edit), and the human-owned `impl
//! …Behavior` bodies. The six cppgc classes are re-exported below so `bindings.rs` (the
//! `extension!` `objects = [...]` list and `getBoundingClientRect`) keeps referring to them
//! unqualified.
//!
//! Pure code move -- no logic change. The `stub!` / `dispatch!` / `dispatch_try!` macros resolve
//! from the crate root (`#[macro_export]` in `behavior_rt`), so `mod core;` must be declared after
//! `#[macro_use] pub mod behavior_rt;` in `lib.rs` — the textual macro scope then reaches these
//! nested files.

use deno_error::JsErrorBox;

pub(crate) mod dom_rect;
pub(crate) mod dom_point;
pub(crate) mod dom_matrix;
pub(crate) mod dom_quad;
pub(crate) mod gfx_transform;


/// Shared `SyntaxError` helper. Used by `dom_matrix` (the CSS-transform parser) and by the DOM
/// selector methods in `bindings.rs`, so it lives at the geometry module root.
///
/// The class name is NOT `"SyntaxError"`: the error-builder registration deliberately skips the JS
/// builtins (their constructors must be preserved), so `JsErrorBox::new("SyntaxError")` surfaces as a
/// plain ECMAScript `SyntaxError`. Every caller here wants the *DOM* SyntaxError —
/// `assert_throws_dom('SyntaxError', …)` checks for a DOMException with a `code` of 12 — so we go
/// through a private class name that the registration maps to `new DOMException(msg, "SyntaxError")`.
/// `.name` is unchanged either way; only the brand and `code` are fixed.
///
/// The NAME lives once, in `core/dom/dom_exception.rs` beside the codes and the registration that
/// reads it; this is an alias so every existing `syntax_err` call site keeps its spelling.
pub(crate) use crate::core::dom::dom_exception::DOM_SYNTAX_ERROR_CLASS as DOM_SYNTAX_ERR;
pub(crate) fn syntax_err(m: &str) -> JsErrorBox { JsErrorBox::new(DOM_SYNTAX_ERR, m.to_string()) }

/// Blink's `geometry_util::NanSafeMin` (`core/geometry/geometry_util.h:18-23`): NaN when either side is NaN, as
/// `Math.min`, else `std::min` -- `f64::min` returns the OTHER side of a NaN, so `new DOMRect(1, 2, NaN, 4).left` was 1
/// where Chrome 150 answers NaN. `std::min(a, b)` is `b < a ? b : a`, which also settles `-0` against `0` as Blink does.
pub(crate) fn nan_safe_min(a: f64, b: f64) -> f64 { if a.is_nan() || b.is_nan() { f64::NAN } else if b < a { b } else { a } }
/// Blink's `geometry_util::NanSafeMax` (`geometry_util.h:27-32`): NaN when either side is NaN, else `std::max`,
/// `a < b ? b : a`.
pub(crate) fn nan_safe_max(a: f64, b: f64) -> f64 { if a.is_nan() || b.is_nan() { f64::NAN } else if a < b { b } else { a } }
