//! `core/navigator/credentials.rs` -- Credential Management as Blink answers it without a credential
//! store to ask (the Chrome oracle ratchet).
//!
//! Every row was a generated stub: `navigator.credentials.get()` resolved `null`, `create()` resolved
//! `undefined`, and `new PasswordCredential({id: "i", password: "p"}).password` read `""`. Chrome 150:
//! - `get()` rejects NotSupportedError "No credential type was specified in the request." when the
//!   options name no credential type (`authentication_credentials_container.cc`), and `create()` when
//!   they do not name exactly one ("Only exactly one of 'password', 'federated', and 'publicKey'
//!   credential types are currently supported.");
//! - `preventSilentAccess()` resolves in a top-level document; a FRAME's request finds no Credential
//!   Management service and rejects NotSupportedError "Error connecting to Credential Management
//!   service." (`scoped_promise_resolver.cc`, measured in an about:blank iframe);
//! - the two constructors validate as `PasswordCredential::Create` / `FederatedCredential::Create`
//!   ('id' / 'password' / 'provider' must not be empty, an invalid URL is a SyntaxError) and keep
//!   `id`, `type`, `name`, `iconURL`, `password`, and the provider's ORIGIN (no trailing slash).

use deno_core::v8;

use crate::binding::protos::{get_prop, make_dom_exc, str_val, throw_dom, throw_type_error};
use crate::install::MemberDef;

fn key<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, &format!("korvus_cred_{name}")).unwrap();
    v8::Private::for_api(scope, Some(n))
}
fn set(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, name: &str, v: v8::Local<v8::Value>) {
    let k = key(scope, name);
    o.set_private(scope, k, v);
}
fn get<'s>(scope: &mut v8::PinScope<'s, '_>, o: v8::Local<v8::Object>, name: &str) -> Option<v8::Local<'s, v8::Value>> {
    let k = key(scope, name);
    o.get_private(scope, k).filter(|v| !v.is_undefined())
}

fn dict_str(scope: &mut v8::PinScope, d: Option<v8::Local<v8::Object>>, k: &str) -> Result<Option<String>, ()> {
    let Some(d) = d else { return Ok(None) };
    let Some(v) = get_prop(scope, d, k) else { return Err(()) };
    if v.is_undefined() {
        return Ok(None);
    }
    let Some(s) = v.to_string(scope) else { return Err(()) };
    Ok(Some(s.to_rust_string_lossy(scope)))
}

/// `Credential::ParseStringAsURLOrThrow`: empty is no URL; an unparseable one is a SyntaxError.
fn parse_url(scope: &mut v8::PinScope, ctx: &str, s: &str) -> Result<Option<url::Url>, ()> {
    if s.is_empty() {
        return Ok(None);
    }
    match url::Url::parse(s) {
        Ok(u) => Ok(Some(u)),
        Err(_) => {
            throw_dom(scope, &format!("{ctx}'{s}' is not a valid URL."), "SyntaxError");
            Err(())
        }
    }
}

/// `new PasswordCredential(data)` / `new FederatedCredential(init)`.
pub(crate) fn credential_init(scope: &mut v8::PinScope, iface: &str, this: v8::Local<v8::Object>, args: &v8::FunctionCallbackArguments) {
    let ctx = format!("Failed to construct '{iface}': ");
    let a0 = args.get(0);
    // PasswordCredential(HTMLFormElement) is the other overload; this engine reads the dictionary one.
    let d = v8::Local::<v8::Object>::try_from(a0).ok();
    let Ok(id) = dict_str(scope, d, "id") else { return };
    let Ok(name) = dict_str(scope, d, "name") else { return };
    let Ok(icon) = dict_str(scope, d, "iconURL") else { return };
    let (secret_key, missing_msg) = if iface == "PasswordCredential" { ("password", "PasswordCredentialData") } else { ("provider", "FederatedCredentialInit") };
    let Ok(secret) = dict_str(scope, d, secret_key) else { return };
    if id.is_none() {
        throw_type_error(scope, &format!("{ctx}Failed to read the 'id' property from '{missing_msg}': Required member is undefined."));
        return;
    }
    if secret.is_none() {
        throw_type_error(scope, &format!("{ctx}Failed to read the '{secret_key}' property from '{missing_msg}': Required member is undefined."));
        return;
    }
    let (id, secret) = (id.unwrap_or_default(), secret.unwrap_or_default());
    if id.is_empty() {
        throw_type_error(scope, &format!("{ctx}'id' must not be empty."));
        return;
    }
    if secret.is_empty() {
        throw_type_error(scope, &format!("{ctx}'{secret_key}' must not be empty."));
        return;
    }
    let icon_url = match icon.as_deref() {
        Some(s) => match parse_url(scope, &ctx, s) {
            Ok(u) => u.map(|u| u.to_string()).unwrap_or_default(),
            Err(()) => return,
        },
        None => String::new(),
    };
    let v = str_val(scope, &id);
    set(scope, this, "id", v);
    let ty = if iface == "PasswordCredential" { "password" } else { "federated" };
    let v = str_val(scope, ty);
    set(scope, this, "type", v);
    let v = str_val(scope, name.as_deref().unwrap_or(""));
    set(scope, this, "name", v);
    let v = str_val(scope, &icon_url);
    set(scope, this, "iconURL", v);
    if iface == "PasswordCredential" {
        let v = str_val(scope, &secret);
        set(scope, this, "password", v);
    } else {
        let Ok(u) = parse_url(scope, &ctx, &secret) else { return };
        let origin = u.map(|u| u.origin().ascii_serialization()).unwrap_or_default();
        let v = str_val(scope, &origin);
        set(scope, this, "provider", v);
        let v = str_val(scope, "");
        set(scope, this, "protocol", v);
    }
}

fn cb_attr(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let this = args.this();
    match get(scope, this, &name) {
        Some(v) => rv.set(v),
        None => {
            if crate::binding::protos::implements(scope, this.into(), "Credential") {
                rv.set(str_val(scope, ""));
            } else {
                throw_type_error(scope, "Illegal invocation");
            }
        }
    }
}

fn reject<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str, msg: &str) -> Option<v8::Local<'s, v8::Value>> {
    let p = v8::PromiseResolver::new(scope)?;
    let e = make_dom_exc(scope, msg, name)?;
    p.reject(scope, e);
    Some(p.get_promise(scope).into())
}

fn resolve<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<v8::Value>) -> Option<v8::Local<'s, v8::Value>> {
    let p = v8::PromiseResolver::new(scope)?;
    p.resolve(scope, v);
    Some(p.get_promise(scope).into())
}

const TYPES: &[&str] = &["password", "federated", "publicKey", "identity", "otp", "digital"];

fn cb_container(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let method = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let this = args.this();
    if !crate::binding::protos::implements(scope, this.into(), "CredentialsContainer") {
        let p = crate::binding::protos::rejected_type_error(scope, "Illegal invocation");
        rv.set(p);
        return;
    }
    // `CredentialRequestOptions` (get) / `CredentialCreationOptions` (create), converted as WebIDL converts it (Chrome
    // 150): every member read once in WebIDL's order, a nested dictionary converted in turn; a
    // refusal rejects (korvus read `signal` first, then the six credential types in its own order)
    let dict = match method.as_str() {
        "get" => Some("CredentialRequestOptions"),
        "create" => Some("CredentialCreationOptions"),
        _ => None,
    };
    let opts = match dict {
        Some(d) => match crate::install::ctor_dicts::convert_dict_caught(scope, args.get(0), d, &format!("Failed to execute '{method}' on 'CredentialsContainer': ")) {
            Ok(o) => Some(o),
            Err(e) => {
                let p = crate::binding::protos::rejected_promise(scope, e);
                rv.set(p);
                return;
            }
        },
        // `preventSilentAccess()` takes no argument and `store()` a Credential: neither has options to read (korvus read
        // a signal and the six credential types off whatever came first)
        None => None,
    };
    // options.signal already aborted rejects with its reason, first.
    if let Some(sig) = opts.and_then(|o| get_prop(scope, o, "signal")).filter(|s| s.is_object()) {
        if let Some(r) = crate::core::dom::abort_controller::aborted_reason(scope, sig) {
            if let Some(p) = v8::PromiseResolver::new(scope) {
                p.reject(scope, r);
                rv.set(p.get_promise(scope).into());
            }
            return;
        }
    }
    let named: usize = match opts {
        Some(o) => {
            let mut n = 0;
            // the CONVERTED options: `password` is a boolean that is always present (false by default), so it names a
            // type only when true; the others are dictionaries present when the page gave them
            for t in TYPES {
                let named = match get_prop(scope, o, t) {
                    Some(v) if *t == "password" => v.is_true(),
                    Some(v) => !v.is_null_or_undefined(),
                    None => false,
                };
                if named {
                    n += 1;
                }
            }
            n
        }
        None => 0,
    };
    let out = match method.as_str() {
        "get" if named == 0 => reject(scope, "NotSupportedError", "No credential type was specified in the request."),
        "create" if named != 1 => reject(scope, "NotSupportedError", "Only exactly one of 'password', 'federated', and 'publicKey' credential types are currently supported."),
        "preventSilentAccess" if !crate::core::html::browsing_context::realm_is_top_document(scope) => {
            reject(scope, "NotSupportedError", "Error connecting to Credential Management service.")
        }
        "get" | "create" => {
            // There is no credential store here: nothing is returned for a request that names one.
            let n = v8::null(scope).into();
            resolve(scope, n)
        }
        _ => {
            let u = v8::undefined(scope).into();
            resolve(scope, u)
        }
    };
    if let Some(p) = out {
        rv.set(p);
    }
}

/// The table body of the Credential rows this file implements.
pub fn install_credentials(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let iface = member.label.split("::").next().unwrap_or("");
    let name = str_val(scope, member.name);
    if iface == "CredentialsContainer" {
        crate::core::dom::shared::row_method_data(scope, proto, member, cb_container, name);
    } else {
        crate::core::dom::shared::row_getter_data(scope, proto, member, cb_attr, name);
    }
}
