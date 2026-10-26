//! `core/html/screen_orientation.rs` -- the object `screen.orientation` hands back.
//!
//! WHAT IT FIXES. `screen.orientation` answered `undefined`. Real Brave 151.0.7922.173 answers a
//! `ScreenOrientation` -- measured over CDP on this box, headed under Xvfb, 2026-09-19:
//!
//! ```text
//!   typeof screen.orientation           "object"
//!   Object.prototype.toString.call(…)   "[object ScreenOrientation]"
//!   .type / .angle                      "landscape-primary" / 0
//!   .onchange                           null            (accessor pair, settable)
//!   Object.getOwnPropertyNames(…)       []              <- EVERY member is on the prototype
//!   screen.orientation === screen.orientation   true    <- ONE object per realm, not one per read
//!   Object.getPrototypeOf(…)            ScreenOrientation.prototype  (chain: → EventTarget → Object)
//!   frames[0].screen.orientation        a DIFFERENT object, on the FRAME's ScreenOrientation.prototype
//! ```
//!
//! It is three reads per page load for a behavioural sensor, and `undefined` is not a value any browser
//! gives, which is why it is a first-tier tell rather than a missing feature.
//!
//! WHERE THE VALUES COME FROM. The worn bundle's `spec::ScreenProfile`, read at CALL time like every
//! other persona row -- nothing here spells an orientation. A profile that captured none (empty
//! `orientation_type`) gets NO object: `screen.orientation` stays `undefined` rather than claiming a
//! landscape desktop the capture never recorded. See `spec::ScreenProfile`'s field comment.
//!
//! THE SHAPE, AND WHY IT IS NOT OWN PROPERTIES. `type` and `angle` are read back through the
//! registry's BRANDED slot getter (`kits::evt_get`, claimed for these two rows in
//! `tools/sync/gen_shells.py`) -- prototype accessors over the instance's private slot, behind the
//! interface's brand, the shape `ResizeObserverSize` and `SubmitEvent.submitter` use. The
//! alternative, own data properties on the minted object the way `navigator.userAgentData` still
//! carries them, would put `["type","angle"]` where Chrome answers `[]` -- trading an `undefined`
//! tell for an enumeration tell.
//!
//! THE SLOTS ARE REWRITTEN ON EVERY READ, not only at mint: the object's identity has to be stable
//! (a page comparing two reads gets `true` in Chrome) while its values stay the persona's live
//! answer. Caching the object without refreshing it would freeze the first read's values into a
//! realm for as long as it lives.

use deno_core::v8;

use crate::binding::protos::str_val;
use crate::core::events::event::evt_slot_write;
use crate::install::live::parent_of;

/// The private slot on the realm's GLOBAL that holds that realm's one `ScreenOrientation`.
///
/// Per realm, and on the global rather than in `OpState`, because a frame has its own
/// `ScreenOrientation.prototype`: one instance cached process-wide would hand a child frame an object
/// prototyped in the parent, which `instanceof` reads in one line. Chrome mints one per realm
/// (measured: `frames[0].screen.orientation !== screen.orientation`, and the frame's is on the
/// frame's prototype).
const CACHE_SLOT: &str = "korvus_screen_orientation";

/// The worn persona's screen, or `None` in a realm with no persona service.
fn screen_profile(scope: &mut v8::PinScope) -> Option<spec::ScreenProfile> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let p = crate::binding::protos::persona_handle(&st)?;
    Some(p.0.screen())
}

/// `screen.orientation` -- this realm's `ScreenOrientation`, carrying the worn bundle's values.
///
/// `undefined` when the profile captured no orientation, and when the realm does not expose the
/// interface (a worker: `ScreenOrientation` is `[Exposed=Window]`, and so is `screen` itself).
pub fn screen_orientation<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Value> {
    let Some(s) = screen_profile(scope) else { return v8::undefined(scope).into() };
    if s.orientation_type.is_empty() {
        // NOT CAPTURED. No fallback exists that would not be an invention -- see the field comment in
        // `spec::ScreenProfile`.
        return v8::undefined(scope).into();
    }
    let global = scope.get_current_context().global(scope);
    let key = {
        let n = v8::String::new(scope, CACHE_SLOT).unwrap();
        v8::Private::for_api(scope, Some(n))
    };
    let obj = match global.get_private(scope, key).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        Some(o) => o,
        None => {
            // The REALM's own `ScreenOrientation.prototype`, so `instanceof`, the `[object
            // ScreenOrientation]` tag and the `EventTarget` half of the chain are the page's own.
            // A realm that does not expose `ScreenOrientation` gets `undefined` rather than an
            // object left on `Object.prototype` -- `persona_objects.rs::mint_persona` takes the second
            // route for `navigator.userAgentData`, and it hands the page an object whose
            // `Object.prototype.toString` reads `[object Object]`, which no browser answers.
            let Some((proto, _)) = parent_of(scope, "ScreenOrientation") else {
                return v8::undefined(scope).into();
            };
            // made from the interface's template
            let o = crate::binding::protos::mint_on(scope, proto.into());
            // THE BRAND, and it is not decoration. It does two jobs, both measured here:
            //
            //  1. `ScreenOrientation`'s rows are registry rows, so they carry the D2 stub's STRICT
            //     receiver check -- the prototype in the chain AND the brand. Without the brand
            //     `screen.orientation.unlock()` threw `TypeError: Illegal invocation` ON A REAL
            //     INSTANCE: the check doing its job to an object with no way to say it was genuine.
            //  2. It is what lets `angle` and `type` be the BRANDED slot getter (`kits::evt_get`,
            //     claimed in `tools/sync/gen_shells.py`) instead of the unbranded one. The unbranded
            //     `slot_get` answers `undefined` to `Object.create(ScreenOrientation.prototype)`,
            //     which is WEAKER than the stub it replaces and weaker than the browser -- the
            //     bundle records 432 `Object.create(I.prototype)` receivers throwing. The installer's
            //     golden surface caught exactly that, which is what the fixture is for.
            crate::install::webidl::brand(scope, o, "ScreenOrientation");
            let ov: v8::Local<v8::Value> = o.into();
            global.set_private(scope, key, ov);
            o
        }
    };
    let ty = str_val(scope, &s.orientation_type);
    evt_slot_write(scope, obj, "type", ty);
    let angle: v8::Local<v8::Value> = v8::Number::new(scope, s.orientation_angle as f64).into();
    evt_slot_write(scope, obj, "angle", angle);
    obj.into()
}
