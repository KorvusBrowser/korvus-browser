//! The geometry interfaces -- `DOMPointReadOnly`/`DOMPoint`, `DOMRectReadOnly`/`DOMRect`, `DOMQuad` and
//! `DOMMatrixReadOnly`/`DOMMatrix` -- from their tables, over their OWN classes.
//!
//! They were deno_core `#[op2]` classes: deno_core built a class template per realm, the bootstrap put
//! the class on the global, `guard_native_class_ctors` wrapped its constructor, and `verify_native`
//! (deleted with them) checked the realised prototype against the table and FINISHED it (the tag, the WebIDL lengths, the
//! constructor chain, an arity guard, an `unrestricted double` setter). Every one of those wrappers was a
//! function no template made. Now `#[webapi(own)]` generates each member's callback and the table's
//! `cppgc` rows name them (`interface!`'s `class` word), so an interface is built exactly like any table
//! interface: [`build_newable`] over the class's constructor callback. The TemplateBuilder records it,
//! and a frame or a worker gets its own from the isolate's template.
//!
//! A realm's member list is its capture's (`installer::join`): the worker capture has no
//! `DOMMatrixReadOnly.toString` and no `DOMMatrix.setMatrixValue`, so the worker's install leaves them
//! out -- the template realised them before, and the worker install had to delete them afterwards.

use deno_core::v8;

use super::rows::build_newable;
use crate::core::geometry::dom_matrix::{DOMMatrix, DOMMatrixReadOnly, DOMMATRIX, DOMMATRIXREADONLY};
use crate::core::geometry::dom_point::{DOMPoint, DOMPointReadOnly, DOMPOINT, DOMPOINTREADONLY};
use crate::core::geometry::dom_quad::{DOMQuad, DOMQUAD};
use crate::core::geometry::dom_rect::{DOMRect, DOMRectReadOnly, DOMRECT, DOMRECTREADONLY};
use crate::install::installer;

/// Whether `name` is one of the geometry interfaces [`install_geometry_interface`] builds.
pub(crate) fn is_geometry(name: &str) -> bool {
    matches!(name, "DOMPointReadOnly" | "DOMPoint" | "DOMRectReadOnly" | "DOMRect" | "DOMQuad" | "DOMMatrixReadOnly" | "DOMMatrix")
}

/// Build one geometry interface on the calling realm's global, in the slot the bootstrap gives it (a
/// realm's global key order is fingerprint surface, so the caller keeps the order the classes had).
/// PARENT BEFORE CHILD: a child's chain is spliced onto the parent's interface object by name.
pub(crate) fn install_geometry_interface<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) {
    let global = scope.get_current_context().global(scope);
    let mut ledger = installer::take_ledger(scope);
    let l = &mut ledger;
    match name {
        "DOMPointReadOnly" => build_newable(scope, global, &DOMPOINTREADONLY, DOMPointReadOnly::__kc_ctor, None, None, l),
        "DOMPoint" => build_newable(scope, global, &DOMPOINT, DOMPoint::__kc_ctor, Some("DOMPointReadOnly"), None, l),
        "DOMRectReadOnly" => build_newable(scope, global, &DOMRECTREADONLY, DOMRectReadOnly::__kc_ctor, None, None, l),
        "DOMRect" => build_newable(scope, global, &DOMRECT, DOMRect::__kc_ctor, Some("DOMRectReadOnly"), None, l),
        "DOMQuad" => build_newable(scope, global, &DOMQUAD, DOMQuad::__kc_ctor, None, None, l),
        "DOMMatrixReadOnly" => build_newable(scope, global, &DOMMATRIXREADONLY, DOMMatrixReadOnly::__kc_ctor, None, None, l),
        "DOMMatrix" => build_newable(scope, global, &DOMMATRIX, DOMMatrix::__kc_ctor, Some("DOMMatrixReadOnly"), None, l),
        _ => {}
    }
    installer::put_ledger(scope, ledger);
}
