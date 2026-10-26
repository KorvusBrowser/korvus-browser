//! `core/html/document_pip.rs` — `DocumentPictureInPicture` and the `documentPictureInPicture`
//! singleton.
//!
//! # Why FP5 deferred it, and why it lands now
//!
//! FP5's report named one value: `documentPictureInPicture.window` must be **`null`**, not
//! `undefined`, and the registry's D2 stub for an `any`-typed readonly attribute answers `undefined`.
//! That is a one-character difference a page reads with `=== null`, and it is the whole reason this
//! name waited for a table.
//!
//! The rest of the interface is a REFUSAL, and a faithful refusal is real behaviour rather than a
//! stub (the rule FP5's own `queryLocalFonts` and the three file pickers established): every
//! `requestWindow()` on the device rejects with `NotAllowedError: Document PiP requires user
//! activation`, because opening a picture-in-picture window needs a user gesture — and this engine
//! never has user activation at all, so the same answer comes from the same cause.
//!
//! # WHAT THE DEVICE ANSWERS — Chrome 150.0.7871.46, Brave 151.1.93.138, Edge 151.0.4129.93 and
//! Opera 135.0.5973.133, headed over CDP under Xvfb, 2026-09-19. All four identical.
//!
//! ```text
//! documentPictureInPicture      own accessor, `get documentPictureInPicture` only, e=1 c=1
//! Object.prototype.toString     [object DocumentPictureInPicture]
//! prototype chain               DocumentPictureInPicture -> EventTarget -> Object
//! .window                       null          (and `typeof` is "object", not "undefined")
//! .onenter                      null, a get/set pair e=1 c=1, assignable and clearable
//! requestWindow()               a Promise that REJECTS, NotAllowedError:
//!   "Failed to execute 'requestWindow' on 'DocumentPictureInPicture': Document PiP requires user
//!    activation"
//! foreign receiver, requestWindow   a REJECTED promise, TypeError:
//!   "Failed to execute 'requestWindow' on 'DocumentPictureInPicture': Illegal invocation"
//! foreign receiver, .window getter  TypeError: Illegal invocation      (thrown, not rejected)
//! new DocumentPictureInPicture()    TypeError: Failed to construct ...: Illegal constructor
//! ```
//!
//! # Exposure
//!
//! **`[SecureContext]`**, measured (FP5.9's whole-window reading, re-measured for this lane):
//! `documentPictureInPicture` is an own property of `window` on `http://127.0.0.1:<p>/` and is GONE
//! on `http://web-platform.test:<p>/` — `isSecureContext` false — on all four browsers. It is one of
//! the 248 names in `tools/sync/data/secure_context.json`, listed for all four, and the interface
//! object `DocumentPictureInPicture` is there beside it, which is why the generated row this table
//! replaces already carried `Exposure::SecureContext` and why the table's header carries `secure`.
//!
//! It is also the name `core/html/exposure.rs` records as one of the five the engine did not gate
//! because it did not INSTALL it: with the singleton here, that shortfall closes.

use deno_core::v8;

use crate::binding::protos::{make_dom_exc, throw_type_error};
use crate::install::installer::{self, Flavor};
use crate::install::MemberDef;
use crate::install::kits::branded;


/// `.window` — **`null`**, and that single value is why FP5 deferred this name: a page asks
/// `documentPictureInPicture.window === null` and a stub's `undefined` fails it. There is no
/// picture-in-picture window here, which is the same state the device reports before one is opened.
fn cb_window(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !branded(scope, &args) {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    rv.set_null();
}

/// `requestWindow(options?)` — the device's refusal, for the device's reason. It REJECTS rather than
/// throwing, including for a foreign receiver: the operation returns a `Promise`, so WebIDL turns its
/// own receiver error into a rejection, which is why
/// `DocumentPictureInPicture.prototype.requestWindow.call({})` hands back a Promise on the device.
fn cb_request_window<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue<'s>) {
    let Some(resolver) = v8::PromiseResolver::new(scope) else { return };
    rv.set(resolver.get_promise(scope).into());
    if !branded(scope, &args) {
        let msg = "Failed to execute 'requestWindow' on 'DocumentPictureInPicture': Illegal invocation";
        // Blink's context applied after the error is made
        let e = deno_core::error::native_error(scope, "TypeError", msg);
        resolver.reject(scope, e);
        return;
    }
    let msg = "Failed to execute 'requestWindow' on 'DocumentPictureInPicture': Document PiP requires user activation";
    if let Some(e) = make_dom_exc(scope, msg, "NotAllowedError") {
        resolver.reject(scope, e);
    }
}

/// Mount one `DocumentPictureInPicture` row. `onenter` is a handler row and goes through
/// `install::kits::event_handler` from the table, never here.
pub fn install_document_pip(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "window" => {
            let g = crate::install::kits::proto_data_fn(scope, proto, cb_window, "get window", 0);
            crate::install::kits::define_accessor(scope, proto, member, g, None);
        }
        // A PROMISE-returning row does its own receiver check, so it has to say so:
        // `install::receiver`'s derived pass wraps anything un-stamped with a guard that THROWS
        // `Illegal invocation`, and that turns the rejection every page's `.catch` is written for
        // into a synchronous throw. `install::webidl`'s own promise rows stamp for the same reason
        // and record the same defect.
        "requestWindow" => {
            let f = crate::install::kits::proto_data_fn(scope, proto, cb_request_window, member.name, member.length as i32);
            crate::binding::protos::stamp_receiver_guarded(scope, f);
            crate::install::kits::define_value(scope, proto, member, f.into(), true);
        }
        other => debug_assert!(false, "install_document_pip: unexpected member {other}"),
    }
}

/// The interface from its table, spliced onto `EventTarget`, and the one singleton.
pub fn install_window_document_pip<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::core::html::exposure::{gate, Exposure, Publishes};
    use crate::core::html::window_globals::{define_window_singleton, mint_shell, Mint, WindowSetter};

    let parent = crate::install::live::rows::parent_of(scope, "EventTarget");
    let mut ledger = installer::take_ledger(scope);
    let built = installer::create_interface(
        scope,
        global,
        &DOCUMENTPICTUREINPICTURE,
        DOCUMENTPICTUREINPICTURE.name,
        parent,
        None,
        Flavor::Table,
        &mut ledger,
    );
    for g in &built.gaps {
        eprintln!("[korvus] document-pip install gap: {}.{}", g.interface, g.member);
    }
    installer::put_ledger(scope, ledger);

    if !gate(scope, Publishes::Global, "documentPictureInPicture", Exposure::SecureContext) {
        return;
    }
    let Some(o) = mint_shell(scope, "DocumentPictureInPicture") else { return };
    define_window_singleton(scope, global, "documentPictureInPicture", o.into(), WindowSetter::None, true, Mint::Shell("DocumentPictureInPicture"));
}

// The table. All four captures record `window`, `onenter`, `requestWindow` in that order with
// `ctorIndex` 3; `window` is a getter-only accessor, `onenter` a get/set pair, `requestWindow` a
// method of length 0, all enumerable and configurable. `secure` in the header is the interface
// object's own exposure condition and it is `tools/sync/data/secure_context.json`'s
// reading, not the IDL's claim.
webapi::interface! {
    "DocumentPictureInPicture" : "EventTarget" [interface, window, secure] {
    ctor    "DocumentPictureInPicture"       len 0                = stub,
    attr    "window"        : Any readonly nullable               = install_document_pip,
    handler "onenter"                                             = event_handler,
    method  "requestWindow" : Promise len 0 required 0            = install_document_pip
    }
}
