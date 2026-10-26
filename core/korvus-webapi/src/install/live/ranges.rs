//! `Range` and `Selection`: the two interface objects, and the receiver check their rows carry.

use deno_core::v8;

use crate::install::installer;
use crate::install::InterfaceDef;

/// `Range` (34 rows) and `Selection` (29) from their `interface!` tables, in the WINDOW.
///
/// These two were `js/dom/08i-range.js` (83 lines, four `Object.defineProperty` sites) -- the ONLY
/// caller of the 49 `op_range_*` / `op_sel_*` factory ops, every one of which is deleted with it.
/// The bodies did not move: `core/dom/range.rs`'s `cb_range_*` and `core/editing/selection.rs`'s
/// `cb_sel_*` are the same callbacks the ops used to wrap, named directly by the rows now.
///
/// NEITHER has a JS class behind it any more. `Range` is built by `build_newable` over
/// `core/dom/range.rs`'s native constructor, which registers each instance in the Rust live-range
/// registry; `Selection` is built by `create_interface` as the non-newable interface WebIDL declares,
/// and the one selection the realm has is minted on its prototype by `the_selection`. So the
/// prototypes are CREATED here rather than taken from a class through `overwrite_prototype`.
///
/// WHY THIS RUNS AFTER THE WHOLE ESM LAYER AND AFTER `BOOTSTRAP` -- the LAST writer, and the SOLE
/// one. The one thing that used to write these prototypes later was `BOOTSTRAP`'s "Range geometry"
/// block (`core/korvus-runtime/src/lib.rs`), which ASSIGNED `op_helper_method` bodies over
/// `getBoundingClientRect` and `getClientRects` -- two `KB.__` bridge slots and two native masks --
/// because this file's rows answered a constant `DOMRect(0,0,0,0)` and a bare `Array` while an
/// element's boxes were real. That block is DELETED in the same commit as this install: its
/// approximation (a collapsed range covers nothing; a non-collapsed one delegates to the closest
/// ELEMENT ancestor of its common container) is `range_box_element` in `core/dom/range.rs` now, so
/// the two rows ARE the bodies and there is no install-order compromise left to keep -- the shape L1
/// had to accept for `HTMLLinkElement.sheet` and L12 for `CSSStyleDeclaration.cssText`.
///
/// Nothing ELSE in the tree defines a member of either interface: every `.js` under
/// `core/korvus-webapi/src/js` and `core/korvus-runtime/src/js`, plus the `lib.rs`/`worker.rs` string
/// literals, were grepped for `Range` and `Selection` -- and since W4.e deleted `host.js`'s two class
/// bodies there is not a single hit outside this file and the two tables.
///
/// Window-only: the bundle's `worker.surface.json` lists neither, and both leave the generated
/// registry with the tables, so this is their only source.
pub fn install_window_ranges<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::core::dom::range::RANGE;
    use crate::core::dom::static_range::STATICRANGE;
    use crate::core::editing::selection::SELECTION;

    use super::rows::build_newable;
    use crate::core::dom::range::cb_range_ctor;
    use crate::core::dom::static_range::cb_static_range_ctor;

    let mut ledger = installer::take_ledger(scope);
    // Chrome 152's two new `AbstractRange` children, first: `NodeRange` (the containers' owner, the parent
    // `Range` and `StaticRange` take now) and `OpaqueRange` (a text control's value range). Both are
    // non-constructible interfaces, as `AbstractRange` is (`abstract_range.rs`).
    {
        use crate::core::dom::abstract_range::{NODERANGE, OPAQUERANGE};
        // the realm's own `AbstractRange` (made by `install_window_abort_style`, earlier in the block), never the
        // page-writable global
        let ctor = crate::binding::protos::iface_object(scope, "AbstractRange").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok());
        let parent = ctor.and_then(|c| {
            let k = v8::String::new(scope, "prototype")?;
            let p = c.get(scope, k.into()).and_then(|p| v8::Local::<v8::Object>::try_from(p).ok())?;
            Some((p, c))
        });
        for def in [&NODERANGE, &OPAQUERANGE] {
            let built = installer::create_interface(scope, global, def, def.name, parent, None, installer::Flavor::Table, &mut ledger);
            for g in &built.gaps {
                eprintln!("[korvus] range install gap: {}.{}", g.interface, g.member);
            }
        }
    }
    // A NATIVE constructor over a fresh prototype, spliced onto `NodeRange` (Chrome 152; `AbstractRange` before). Until
    // this unit `Range` was `host.js`'s JS class and this call was `take_prototype` -- the table
    // owned the members, the class owned the identity, and `new Range()` ran JS that registered the
    // instance in the JS live-range set. Both halves are Rust now (`core/dom/range.rs`), so nothing
    // is left to take a prototype FROM.
    build_newable(scope, global, &RANGE, cb_range_ctor, Some("NodeRange"), None, &mut ledger);
    // `StaticRange`, the OTHER `AbstractRange` child, beside `Range` because they are the
    // same shape -- a native constructor over a fresh prototype spliced onto `AbstractRange`, whose
    // four boundary slots both write and whose accessors both inherit. It was `host.js`'s
    // `class StaticRange extends globalThis.AbstractRange` and W4.e's last remains; the registry row
    // `I98` that used to shadow it leaves the generated registry with the table (invariant I1).
    //
    // The table carries NO rows, and that is the capture: `StaticRange.prototype`'s only own key is
    // `constructor`, because every boundary member is `AbstractRange.prototype`'s.
    build_newable(scope, global, &STATICRANGE, cb_static_range_ctor, Some("NodeRange"), None, &mut ledger);
    install_selection_interface(scope, global, &SELECTION, &mut ledger);
    installer::put_ledger(scope, ledger);
}

/// `globalThis.Selection`: a genuinely native, NON-newable interface object over a FRESH prototype,
/// with the table's rows installed on it.
///
/// The capture records `newable: false` for `Selection` and the bundle's oracle records
/// `new Selection()` as a `TypeError` -- WebIDL declares no constructor, so Chrome 150 answers
/// `TypeError: Failed to construct 'Selection': Illegal constructor` (measured over loopback), which
/// is exactly what `create_interface` builds for a `Model::Interface` table.
///
/// The prototype is built here rather than TAKEN from a `host.js` class. There is no class
/// left to take it from, and the ONE selection the realm has is minted on this prototype, lazily, by
/// `core/editing/selection.rs`'s `the_selection`.
fn install_selection_interface<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    global: v8::Local<'s, v8::Object>,
    def: &'static InterfaceDef,
    ledger: &mut installer::InstallLedger,
) {
    let built = installer::create_interface(scope, global, def, def.name, None, None, installer::Flavor::Table, ledger);
    for g in &built.gaps {
        eprintln!("[korvus] ranges install gap: {}.{}", g.interface, g.member);
    }
}

// ── the receiver check these two tables' rows carry (closed by W4.e) ─────────────
//
// Measured in Chrome 150 over loopback (2026-09-11): EVERY member of `Range.prototype` and
// `Selection.prototype` -- 25 Range methods, `commonAncestorContainer`'s getter, 17 Selection methods
// and 12 Selection getters -- answers `TypeError: Illegal invocation` for a receiver that is not one
// of its platform objects, and `Object.create(Range.prototype).toString()` throws it too (the check
// is for an internal slot, not for the prototype chain). Measured on THIS tree before the cutover:
// not one of the 42 threw. They answered `undefined` / `false` / `""` / `0`, or did real work on a
// foreign object -- `Range.prototype.createContextualFragment.call({}, "<b>x</b>")` returned a
// DocumentFragment.
//
// WHAT THE BRAND IS. Both interfaces are minted in Rust now -- `Range` by its native
// constructor (`core/dom/range.rs`), `Selection` by `the_selection` (`core/editing/selection.rs`) --
// so the brand is the real thing the event family uses: a `v8::Private` stamped at the mint site and
// nowhere else (`core/events/ctor.rs`'s `brand` / `has_brand`).
//
// That REPLACES the shape this check had while the two were JS classes: "the interface prototype is
// in the receiver's chain, and the receiver carries the storage the class writes". It had to name
// the storage (`Range`'s `startContainer` slot, `Selection`'s `_anchor` property), which made it a
// heuristic with a stated residual -- a page that FORGED `Object.assign(Object.create(Selection
// .prototype), { _anchor: null })` passed here and is rejected by Chrome. A private cannot be read,
// copied or deleted from JS, so that residual is closed and no chain walk is needed to close it.

/// The interfaces whose rows carry the receiver check. The discriminant travels in the guard's data,
/// so ONE callback serves both.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Brand {
    Range,
    Selection,
}

impl Brand {
    fn iface(self) -> &'static str {
        match self {
            Brand::Range => "Range",
            Brand::Selection => "Selection",
        }
    }
    fn from_index(i: u32) -> Self {
        if i == 0 {
            Brand::Range
        } else {
            Brand::Selection
        }
    }
}

/// Whether `obj` is a real instance of `brand`'s interface: it carries the private the interface's
/// one mint site stamps, and nothing else can.
fn brand_ok(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, brand: Brand) -> bool {
    crate::core::events::ctor::has_brand(scope, obj.into(), brand.iface())
}

/// The one guard callback. `data = [brandIndex, originalFn]`.
fn cb_brand_guard(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    let Ok(d) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let idx = d.get_index(scope, 0).and_then(|v| v.uint32_value(scope)).unwrap_or(0);
    let Some(orig) = d.get_index(scope, 1).and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return };
    range_brand_run(scope, &args, rv, idx, orig);
}

/// [`cb_brand_guard`]'s body (the install's wrapper; a template fuses [`range_brand_check`] instead).
pub(crate) fn range_brand_run(
    scope: &mut v8::PinScope,
    args: &v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
    idx: u32,
    orig: v8::Local<v8::Function>,
) {
    if !range_brand_check(scope, args, idx) {
        return;
    }
    let argv: Vec<v8::Local<v8::Value>> = (0..args.length()).map(|i| args.get(i)).collect();
    let this: v8::Local<v8::Value> = args.this().into();
    // `orig` throwing returns None with the exception pending -- leaving `rv` untouched propagates it.
    if let Some(r) = orig.call(scope, this, &argv) {
        rv.set(r);
    }
}

/// [`range_brand_run`]'s check, shared with a fused member: the brand (`false`: thrown).
pub(crate) fn range_brand_check(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, idx: u32) -> bool {
    if !brand_ok(scope, args.this(), Brand::from_index(idx)) {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return false;
    }
    true
}

/// Wrap `orig` in the receiver check, keeping the member's WebIDL `name` and `length` (the wrapper is
/// what the page sees, so its identity has to be the member's).
fn wrap_brand<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    brand: Brand,
    orig: v8::Local<'s, v8::Value>,
    name: &str,
    length: i32,
) -> v8::Local<'s, v8::Function> {
    let d = v8::Array::new(scope, 2);
    let k = v8::Integer::new_from_unsigned(scope, brand as u32);
    d.set_index(scope, 0, k.into());
    d.set_index(scope, 1, orig);
    let g = crate::binding::protos::build_fn_data(scope, cb_brand_guard, name, length, d.into());
    if crate::install::templates::capturing() {
        if let Ok(inner) = v8::Local::<v8::Function>::try_from(orig) {
            let guard = crate::install::templates::Guard::RangeBrand { idx: brand as u32 };
            crate::install::templates::record_guard(scope, g, name, length, guard, inner);
        }
    }
    g
}

/// [`super::rows::method_row`] with the receiver check OUTSIDE the arity guard -- the order Chrome uses:
/// `Range.prototype.setStart.call({})` is `Illegal invocation`, not "2 arguments required".
pub(crate) fn branded_method_row<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    target: v8::Local<v8::Object>,
    member: &'static crate::install::MemberDef,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
    brand: Brand,
) {
    use crate::binding::protos::{build_fn, make_arity_guard};
    let iface = member.label.split("::").next().unwrap_or(member.name);
    let f = build_fn(scope, cb, member.name, member.length as i32);
    let inner: v8::Local<v8::Value> = if member.required > 0 {
        make_arity_guard(scope, iface, member.name, member.required as i32, member.length as i32, f.into(), member.ty).into()
    } else {
        f.into()
    };
    let g = wrap_brand(scope, brand, inner, member.name, member.length as i32);
    crate::install::kits::define_value(scope, target, member, g.into(), true);
}

/// [`super::rows::getter_row`] with the receiver check.
pub(crate) fn branded_getter_row<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    proto: v8::Local<v8::Object>,
    member: &'static crate::install::MemberDef,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
    brand: Brand,
) {
    use crate::binding::protos::build_fn;
    let name = format!("get {}", member.name);
    let f = build_fn(scope, cb, &name, 0);
    let g = wrap_brand(scope, brand, f.into(), &name, 0);
    crate::install::kits::define_accessor(scope, proto, member, g, None);
}
