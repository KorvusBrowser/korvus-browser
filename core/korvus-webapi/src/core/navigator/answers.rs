//! `core/navigator/answers.rs` -- the promise-returning members pages ask FIRST, with the answers a
//! real browser gives on this device.
//!
//! All four were D2 stubs that resolved `undefined`, which no browser ever does for them, and the
//! stub-call counter over the failing replay rows (C3 FIND) found pages calling them: passkey sign-in
//! scripts ask `isUserVerifyingPlatformAuthenticatorAvailable()` and `getClientCapabilities()` before
//! they render, storage libraries probe `navigator.storage.getDirectory()` for OPFS, and a silent
//! `navigator.credentials.get` is how a site checks for a saved password.
//!
//! Measured in Chrome 150 and in the persona, Brave 151, on this host (2026-09-26,
//! `tools/realweb/seed/sx`), which answered identically:
//!
//! * `isUserVerifyingPlatformAuthenticatorAvailable()` resolves `false` (no platform authenticator on
//!   Linux);
//! * `getClientCapabilities()` resolves a plain object of 24 boolean keys, in the order below;
//! * `getDirectory()` resolves the origin private file system's root -- now `core/fileapi/opfs.rs`'s,
//! with the file system behind it;
//! * `credentials.get({ mediation: "silent", password: true })` resolves `null`: nothing saved.
//!
//! A `publicKey` request would show the browser's passkey sheet and wait for a person; with nobody to
//! answer it Chrome rejects with `NotAllowedError` once the sheet is dismissed, which is what this
//! answers at once.

use deno_core::v8;

use crate::binding::protos::str_val;
use crate::install::live::method_row;
use crate::install::MemberDef;

/// `getClientCapabilities()`'s keys and values, in Chrome's order.
const CLIENT_CAPABILITIES: [(&str, bool); 24] = [
    ("conditionalCreate", true),
    ("conditionalGet", true),
    ("extension:appid", true),
    ("extension:appidExclude", true),
    ("extension:cmtgKey", false),
    ("extension:credBlob", true),
    ("extension:credProps", true),
    ("extension:credentialProtectionPolicy", true),
    ("extension:crossDeviceFallbackUrl", false),
    ("extension:enforceCredentialProtectionPolicy", true),
    ("extension:getCredBlob", true),
    ("extension:hmacCreateSecret", true),
    ("extension:largeBlob", true),
    ("extension:minPinLength", true),
    ("extension:payment", false),
    ("extension:prf", true),
    ("hybridTransport", true),
    ("immediateGet", true),
    ("passkeyPlatformAuthenticator", true),
    ("relatedOrigins", true),
    ("signalAllAcceptedCredentials", true),
    ("signalCurrentUserDetails", true),
    ("signalUnknownCredential", true),
    ("userVerifyingPlatformAuthenticator", false),
];

fn resolved(scope: &mut v8::PinScope, rv: &mut v8::ReturnValue, v: v8::Local<v8::Value>) {
    let Some(r) = v8::PromiseResolver::new(scope) else { return };
    r.resolve(scope, v);
    rv.set(r.get_promise(scope).into());
}

fn cb_uvpaa(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let v = v8::Boolean::new(scope, false).into();
    resolved(scope, &mut rv, v);
}

fn cb_client_capabilities(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let o = v8::Object::new(scope);
    for (k, b) in CLIENT_CAPABILITIES {
        let key = str_val(scope, k);
        let v = v8::Boolean::new(scope, b);
        o.set(scope, key, v.into());
    }
    resolved(scope, &mut rv, o.into());
}

fn cb_credentials_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let public_key = v8::Local::<v8::Object>::try_from(args.get(0)).ok().is_some_and(|o| {
        let k = str_val(scope, "publicKey");
        o.get(scope, k).is_some_and(|v| !v.is_undefined())
    });
    let Some(r) = v8::PromiseResolver::new(scope) else { return };
    if public_key {
        if let Some(e) = crate::binding::protos::make_dom_exc(
            scope,
            "The operation either timed out or was not allowed. See: https://www.w3.org/TR/webauthn-2/#sctn-privacy-considerations-client.",
            "NotAllowedError",
        ) {
            r.reject(scope, e);
        }
    } else {
        let n = v8::null(scope).into();
        r.resolve(scope, n);
    }
    rv.set(r.get_promise(scope).into());
}

// (`navigator.storageBuckets.open` and `StorageBucket.name` were here, with the private slot they shared; the
// whole API is `core/navigator/storage_buckets.rs` now.)

fn rejected_dom(scope: &mut v8::PinScope, rv: &mut v8::ReturnValue, msg: &str, name: &str) {
    let Some(r) = v8::PromiseResolver::new(scope) else { return };
    if let Some(e) = crate::binding::protos::make_dom_exc(scope, msg, name) {
        r.reject(scope, e);
    }
    rv.set(r.get_promise(scope).into());
}

/// `navigator.managed.getManagedConfiguration(keys)`: a page that is not a managed web app is refused
/// with NotAllowedError (Chrome 150, measured).
fn cb_managed_config(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rejected_dom(scope, &mut rv, "Service connection error. This API is available only for managed apps.", "NotAllowedError");
}

/// `RemotePlayback.watchAvailability(callback)` resolves the callback's id (Chrome 150 resolves a
/// number); `cancelWatchAvailability(id)` resolves.
fn cb_watch_availability(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let v = v8::Integer::new_from_unsigned(scope, id).into();
    resolved(scope, &mut rv, v);
}

fn cb_cancel_watch(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let u = v8::undefined(scope).into();
    resolved(scope, &mut rv, u);
}

/// The kit (`tools/sync/gen_shells.py` claims the rows as `promise_answers`).
pub(crate) fn install(scope: &mut v8::PinScope, target: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.label {
        "PublicKeyCredential::isUserVerifyingPlatformAuthenticatorAvailable" => method_row(scope, target, member, cb_uvpaa),
        "PublicKeyCredential::getClientCapabilities" => method_row(scope, target, member, cb_client_capabilities),
        "CredentialsContainer::get" => method_row(scope, target, member, cb_credentials_get),
        "NavigatorManagedData::getManagedConfiguration" => method_row(scope, target, member, cb_managed_config),
        "RemotePlayback::watchAvailability" => method_row(scope, target, member, cb_watch_availability),
        "RemotePlayback::cancelWatchAvailability" => method_row(scope, target, member, cb_cancel_watch),
        other => {
            debug_assert!(false, "answers::install: unexpected member {other}");
        }
    }
}
