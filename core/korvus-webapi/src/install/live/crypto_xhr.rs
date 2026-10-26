//! Crypto, messaging, cookies, FileReader and XMLHttpRequest.
//!
//! The ten interfaces `dom/08f-crypto.js` used to install with 22 `Object.defineProperty` sites over
//! ~55 `op_*` builder round-trips, onto the cppgc class templates' own prototypes, behind throwing JS
//! wrappers that SHARED those prototypes. Each is an `interface!` table in its own interface file now
//! (`modules/crypto/{crypto_key,subtle_crypto}.rs`, `core/fileapi/file_reader.rs`,
//! `core/messaging/message_port.rs`, `modules/broadcastchannel/broadcast_channel.rs`,
//! `modules/cookie_store/cookie_store.rs`, `core/xmlhttprequest/xml_http_request.rs`), and this is
//! where THE installer mounts them: the three newable ones through `build_newable`, the seven
//! non-newable ones through `create_interface`.
//!
//! Written; the git log is the rest of the history.

use deno_core::v8;

use super::rows::{build_newable, parent_of};
use crate::install::installer::{self, Flavor};
use crate::install::InterfaceDef;

/// The interfaces of this lane that BOTH captures list, from the tables, in WebIDL inheritance order.
/// `window` also brings the two the window capture alone has (`CookieStore` and its `cookieStore`
/// singleton). `XMLHttpRequest` is in BOTH realms, from one table and two selectors.
fn install_crypto_xhr_interfaces<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, realm: &str, window: bool) {
    use crate::core::fileapi::file_reader::{cb_fr_ctor, FILEREADER};
    use crate::core::messaging::message_port::MESSAGEPORT;
    use crate::core::xmlhttprequest::xml_http_request::{cb_xhr_ctor, XMLHTTPREQUEST, XMLHTTPREQUESTEVENTTARGET, XMLHTTPREQUESTUPLOAD};
    use crate::modules::broadcastchannel::broadcast_channel::{cb_bc_ctor, BROADCASTCHANNEL};
    use crate::modules::cookie_store::cookie_store::COOKIESTORE;
    use crate::modules::crypto::crypto_key::CRYPTOKEY;
    use crate::modules::crypto::subtle_crypto::SUBTLECRYPTO;

    let mut ledger = installer::take_ledger(scope);
    let et = parent_of(scope, "EventTarget");
    let build = |scope: &mut v8::PinScope<'s, '_>,
                     ledger: &mut installer::InstallLedger,
                     def: &'static InterfaceDef,
                     parent: Option<(v8::Local<'s, v8::Object>, v8::Local<'s, v8::Function>)>|
     -> (v8::Local<'s, v8::Object>, v8::Local<'s, v8::Function>) {
        let built = installer::create_interface(scope, global, def, def.name, parent, None, Flavor::Table, ledger);
        for g in &built.gaps {
            eprintln!("[{realm}] crypto/xhr install gap: {}.{}", g.interface, g.member);
        }
        (built.proto, built.ctor)
    };

    build(scope, &mut ledger, &CRYPTOKEY, None);
    build(scope, &mut ledger, &SUBTLECRYPTO, None);
    // The Web Locks pair, both realms, `[SecureContext]` through the rows' `secure`.
    {
        use crate::core::html::web_locks::{LOCK, LOCKMANAGER};
        build(scope, &mut ledger, &LOCK, None);
        build(scope, &mut ledger, &LOCKMANAGER, None);
    }
    build(scope, &mut ledger, &MESSAGEPORT, et);
    let xet = build(scope, &mut ledger, &XMLHTTPREQUESTEVENTTARGET, et);
    build(scope, &mut ledger, &XMLHTTPREQUESTUPLOAD, Some(xet));
    build_newable(scope, global, &FILEREADER, cb_fr_ctor, Some("EventTarget"), None, &mut ledger);
    build_newable(scope, global, &BROADCASTCHANNEL, cb_bc_ctor, Some("EventTarget"), None, &mut ledger);
    if window {
        build(scope, &mut ledger, &COOKIESTORE, et);
        // `navigator.keyboard`'s pair, window-only and `[SecureContext]`.
        {
            use crate::core::navigator::keyboard::{KEYBOARD, KEYBOARDLAYOUTMAP};
            build(scope, &mut ledger, &KEYBOARD, None);
            build(scope, &mut ledger, &KEYBOARDLAYOUTMAP, None);
        }
        build_newable(scope, global, &XMLHTTPREQUEST, cb_xhr_ctor, Some("XMLHttpRequestEventTarget"), None, &mut ledger);
    } else {
        // The WORKER's `XMLHttpRequest` is the SAME table, with the worker selector --
        // the window's twenty-four members minus `responseXML`, which the worker capture does not
        // record. It was the last name in `tools/sync/data/worker_js_owned.json`: a
        // `generated_worker_shells.js` shell whose `open`/`send` were `op_noop_method`s until
        // `worker-native.js` wired twenty-one builder ops behind them, one native class replaced
        // outright, and a prototype re-chained by hand below. All three files are deleted.
    // NO HAND SELECTOR ANY MORE. The realm's member list used to be written down
    // here, copied out of one capture; `install::installer::join` reads it off the ACTIVE
    // capture instead, so the list is derived and a bundle swap moves it.
        build_newable(scope, global, &XMLHTTPREQUEST, cb_xhr_ctor, Some("XMLHttpRequestEventTarget"), None, &mut ledger);
    }

    installer::put_ledger(scope, ledger);

    // `Crypto` -- ALL THREE rows, in BOTH realms, from the one table
    // (`modules/crypto/crypto.rs`). This lane (L10) used to mount `subtle` alone through
    // `CRYPTO_SUBTLE`, because the other two had JS bodies in `shared-globals.js`; a later change gave
    // them Rust bodies, so the interface is one table and this is its only install in either realm.
    // It also strips the `crypto` object literal bare and re-parents it -- what `09i-singletons.js`'s
    // `rehome` did in the window, and what nothing did in a worker.
    crate::modules::crypto::crypto::install_crypto_singleton(scope, global, window);
}

/// The lane's interfaces in the WINDOW, plus the `cookieStore` singleton.
///
/// Called from [`super::install_window_bootstrap`] right after the registry install, which is where the
/// deleted `dom/08f-crypto.js` block used to have run (during `dom.js`) and where the hand-written
/// `XMLHttpRequest` chain fix-up used to sit: after `EventTarget` and `Event` exist and after the
/// registry has created `Crypto`, and before `dom/09i-singletons.js` re-homes the singletons.
pub fn install_window_crypto_xhr<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    install_crypto_xhr_interfaces(scope, global, "korvus", true);

    // `cookieStore`: an own getter-only ACCESSOR of the window holding one native `CookieStore`, as in
    // every browser of the family. It was the data property `globalThis.cookieStore =`
    // produced in the deleted JS.
    //
    // `[SecureContext]`, MEASURED and NOT what the IDL says: webref's
    // `cookiestore.idl` puts no `[SecureContext]` on `Window.cookieStore`, and all four browsers take
    // the name away on `http://web-platform.test:<p>/` regardless. The browser is the authority.
    if crate::core::html::exposure::gate(
        scope,
        crate::core::html::exposure::Publishes::Global,
        "cookieStore",
        crate::core::html::exposure::Exposure::SecureContext,
    ) {
        let store = crate::modules::cookie_store::cookie_store::make_cookie_store(scope);
        crate::core::html::window_globals::define_window_singleton(scope, global, "cookieStore", store.into(), crate::core::html::window_globals::WindowSetter::None, true, crate::core::html::window_globals::Mint::With(mint_cookie_store));
    }
}

/// This realm's `cookieStore`.
fn mint_cookie_store<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Value>> {
    Some(crate::modules::cookie_store::cookie_store::make_cookie_store(scope).into())
}

/// The same tables in the WORKER realm (invariant I8), called from `core/korvus-runtime/src/worker.rs`
/// beside [`super::install_worker_indexeddb`].
///
/// No `CookieStore` and no `cookieStore`: the bundle's `worker.surface.json` has neither, and the
/// bundle is the denominator.
///
/// `XMLHttpRequest` IS THE TABLE HERE TOO now, filtered by the ACTIVE capture's own
/// worker member list (it was the hand-written `WORKER_XHR_MEMBERS`). The hand
/// re-chaining that used to end this function is gone with the shell it was repairing: the interface
/// this call builds names `XMLHttpRequestEventTarget` as its parent and `build_newable` splices it,
/// so nothing chains onto a member-less phantom any more.
pub fn install_worker_crypto_xhr<'s>(scope: &mut v8::PinScope<'s, '_>) {
    let global = scope.get_current_context().global(scope);
    install_crypto_xhr_interfaces(scope, global, "korvus-worker", false);
}

