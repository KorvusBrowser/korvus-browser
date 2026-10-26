//! `core/html/bar_props.rs` — `BarProp` and `Viewport`, the two ONE-MEMBER window objects.
//!
//! Seven of the 43 own globals FP5 measured missing are instances of these two interfaces:
//! `locationbar`, `menubar`, `personalbar`, `scrollbars`, `statusbar`, `toolbar` and `viewport`.
//! Both interfaces were already on the global — the registry publishes them from the capture — and
//! both had exactly one prototype member, mounted as a D2 stub, so an instance minted onto them would
//! have answered `false` where the device answers `true` and `null` where the device answers an array.
//!
//! # Why they are TABLES rather than two lines in `window_extras.rs`
//!
//! Because `tests/install_ledger.rs` is right. FP5 first minted the instances and then redefined
//! `BarProp.prototype.visible` and `Viewport.prototype.segments` from outside; the ledger recorded
//! both as second writers of a slot the installer mounted, which is the invariant ("a member is
//! installed exactly once, by exactly one row") doing its job. A member with a body belongs in the
//! interface's own table, and the table is what takes the interface out of
//! `install/generated_registry.rs` — `tools/sync/table_inventory.py` reads every table header in the
//! tree, so the exclusion is derived and not a list anybody maintains. (That scanner takes the first
//! quoted string after a table header as the interface NAME, which is why this paragraph does not
//! spell one out: a header written inside a doc comment is parsed as a real table, and
//! `scripts/sync.sh` fails with `unparsable row`. Measured, by writing one.)
//!
//! # What the browsers answer, measured for this unit
//!
//! Brave 151.1.93.138 and Chrome 150.0.7871.46, headed, over CDP on a 127.0.0.1 page, 2026-09-19:
//!
//! ```text
//! locationbar … toolbar     six DISTINCT objects (`locationbar !== menubar`), each [object BarProp],
//!                           `visible` true on all six, own descriptor `get X`/`set X` e=1 c=1
//! viewport.segments         a FROZEN array of ONE DOMRect, and a NEW array each read
//!                           (`viewport.segments !== viewport.segments`); the rect is the WINDOW's
//!                           inner box -- {0,0,1920,915} beside a clientWidth of 1905
//! ```
//!
//! THE TWO VIEWPORT OBJECTS DISAGREE ON PURPOSE, and that is the fact a guess gets wrong:
//! `viewport.segments[0].width` is `innerWidth` while `visualViewport.width` is
//! `documentElement.clientWidth`. Both readings come from the same page load of the same browser.

use deno_core::v8;

use crate::install::installer::{self, Flavor};
use crate::install::MemberDef;
use crate::install::kits::brand_getter;
use crate::install::kits::branded;


// ── BarProp ─────────────────────────────────────────────────────────────────────────────────────

/// `visible` is `true` on all six bars of a headed Brave 151 and Chrome 150 window. It is not a
/// constant this engine invented: a window with a location bar, a menu bar and scrollbars is the
/// window this persona describes, and the D2 stub's `false` claimed the opposite.
fn cb_bar_visible(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !branded(scope, &args) {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return;
    }
    let t = v8::Boolean::new(scope, true);
    rv.set(t.into());
}

/// Mount `BarProp`'s one row.
pub fn install_bar_prop(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "visible" => brand_getter(scope, proto, member, cb_bar_visible),
        other => debug_assert!(false, "install_bar_prop: unexpected member {other}"),
    }
}

// ── Viewport ────────────────────────────────────────────────────────────────────────────────────

/// `segments`: a frozen one-element array holding a `DOMRect` of the window's inner box, minted
/// FRESH on every read — `viewport.segments !== viewport.segments` on both browsers, so caching it
/// would be the tell.
fn cb_viewport_segments(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !branded(scope, &args) {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return;
    }
    let num = |scope: &mut v8::PinScope, k: &str| -> f64 {
        crate::binding::protos::global_get(scope, k).and_then(|v| v.number_value(scope)).unwrap_or(0.0)
    };
    let (w, h) = (num(scope, "innerWidth"), num(scope, "innerHeight"));
    let vals: Vec<v8::Local<v8::Value>> = [0.0f64, 0.0, w, h]
        .iter()
        .map(|&n| {
            let v: v8::Local<v8::Value> = v8::Number::new(scope, n).into();
            v
        })
        .collect();
    let Some(rect) = crate::binding::protos::new_global(scope, "DOMRect", &vals) else { return };
    let arr = v8::Array::new(scope, 1);
    arr.set_index(scope, 0, rect.into());
    let obj: v8::Local<v8::Object> = arr.into();
    obj.set_integrity_level(scope, v8::IntegrityLevel::Frozen);
    rv.set(arr.into());
}

/// Mount `Viewport`'s one row.
pub fn install_viewport(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "segments" => brand_getter(scope, proto, member, cb_viewport_segments),
        other => debug_assert!(false, "install_viewport: unexpected member {other}"),
    }
}

/// Both tables installed from their own definitions, and the seven singletons minted onto them.
///
/// Six DISTINCT `BarProp`s — `locationbar !== menubar` on the device — and one `Viewport`. Each is a
/// `[Replaceable]` own accessor on the global (`get X`/`set X`, e=1, c=1), and which of the seven this
/// realm gets is the active capture's, through `exposure::gate`: the engine reads the bundle and never
/// asks which bundle it is wearing.
///
/// All seven are UNCONDITIONAL, measured rather than assumed: every one of them is an
/// own property of `window` on `http://web-platform.test:<p>/` — `isSecureContext` false — on both
/// Chrome 150 and Brave 151, exactly as on a loopback page.
///
/// Position: after the registry install and before
/// [`installer::seat_globals_in_capture_order`], which puts each name where its capture has it.
pub fn install_window_bar_props<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::core::html::exposure::Exposure;
    use crate::core::html::window_globals::{define_window_singleton, mint_shell, Mint, WindowSetter};

    let mut ledger = installer::take_ledger(scope);
    for def in [&BARPROP, &VIEWPORT] {
        let built =
            installer::create_interface(scope, global, def, def.name, None, None, Flavor::Table, &mut ledger);
        for g in &built.gaps {
            eprintln!("[korvus] bar-props install gap: {}.{}", g.interface, g.member);
        }
    }
    installer::put_ledger(scope, ledger);

    for name in ["locationbar", "menubar", "personalbar", "scrollbars", "statusbar", "toolbar"] {
        if !crate::core::html::exposure::gate(scope, crate::core::html::exposure::Publishes::Global, name, Exposure::Unconditional) {
            continue;
        }
        if let Some(o) = mint_shell(scope, "BarProp") {
            define_window_singleton(scope, global, name, o.into(), WindowSetter::Replaceable, true, Mint::Shell("BarProp"));
        }
    }
    if crate::core::html::exposure::gate(scope, crate::core::html::exposure::Publishes::Global, "viewport", Exposure::Unconditional) {
        if let Some(o) = mint_shell(scope, "Viewport") {
            define_window_singleton(scope, global, "viewport", o.into(), WindowSetter::Replaceable, true, Mint::Shell("Viewport"));
        }
    }
}

// The two tables themselves are at the bottom, in the order the capture lists their interfaces.
// Neither is constructible: `new BarProp()` and `new Viewport()` are `Illegal constructor` on both
// browsers, which `Model::Interface` plus a `stub` ctor row spells. Instances are minted by
// `install::live::window_extras` and nowhere else.
webapi::interface! {
    "BarProp" [interface, window] {
    ctor "BarProp" len 0                        = stub,
    attr "visible" : Boolean readonly           = install_bar_prop
    }
}

webapi::interface! {
    "Viewport" [interface, window] {
    ctor "Viewport" len 0                       = stub,
    attr "segments" : Sequence readonly nullable = install_viewport
    }
}
