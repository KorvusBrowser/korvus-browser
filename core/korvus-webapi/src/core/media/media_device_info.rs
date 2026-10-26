//! `core/media/media_device_info.rs` -- `MediaDeviceInfo` and `InputDeviceInfo` as two `interface!` tables
//! (PHASE 3 F7).
//!
//! `navigator.mediaDevices.enumerateDevices()` handed back PLAIN OBJECTS with the four fields as own data
//! properties (`core/navigator/persona_objects.rs`), while both interfaces stayed generated-registry shells
//! whose rows were stubs. Chrome 150 answers instances: `Object.prototype.toString.call(d)` is
//! `[object MediaDeviceInfo]`, `d instanceof MediaDeviceInfo`, `Object.getOwnPropertyNames(d)` is `[]`, the
//! fields are prototype getters, and `toJSON()` exists (measured 2026-10-11, no DevTools; a behavioural sensor reads
//! the class). Blink: modules/mediastream/media_device_info.{idl,cc}, input_device_info.{idl,cc}.
//!
//! Rows are VERBATIM from the two blocks of `install/generated_registry.rs` they replace -- names, order,
//! kinds, types, lengths, the `[SecureContext]` exposure `secure_context.json` measured on `MediaDeviceInfo`
//! and not on `InputDeviceInfo`, and the stub constructors (`Illegal constructor`).
//!
//! An input device is an `InputDeviceInfo` (`MediaDevices::DevicesEnumerated` makes one for `audioinput` and
//! `videoinput`), and every row of the base reads it: the BRAND is always `MediaDeviceInfo`'s, the prototype
//! the device's own -- the arrangement `core/media/media_stream.rs` uses for a canvas capture track.

use deno_core::v8;

use crate::binding::instance_state as st;
use crate::binding::protos::{body_data, shell_proto};
use crate::install::kits::str_data_getter as acc_get;
use crate::install::live::method_row;
use crate::install::MemberDef;

pub(crate) const DEVICE: &str = "MediaDeviceInfo";
const INPUT: &str = "InputDeviceInfo";

/// One device entry, as `enumerateDevices()` lists it: `kind` picks the interface, the three strings are
/// the entry's (all `""` before the page holds a permission).
pub(crate) fn mint_device<'s>(scope: &mut v8::PinScope<'s, '_>, kind: &str, device_id: &str, label: &str, group_id: &str) -> Option<v8::Local<'s, v8::Object>> {
    let iface = if kind == "audioinput" || kind == "videoinput" { INPUT } else { DEVICE };
    let proto = shell_proto(scope, iface)?;
    let o = crate::binding::protos::mint_on(scope, proto.into());
    let s = st::attach(scope, o, DEVICE);
    st::set_str(scope, s, "deviceId", device_id);
    st::set_str(scope, s, "kind", kind);
    st::set_str(scope, s, "label", label);
    st::set_str(scope, s, "groupId", group_id);
    Some(o)
}

/// The four readonly fields, read by name off the state.
fn cb_device_field(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), DEVICE) else { return };
    let name = body_data(scope, &args).to_rust_string_lossy(scope);
    let v = st::get(scope, s, &name);
    rv.set(v);
}

/// `toJSON()`: `MediaDeviceInfo::toJSONForBinding`, the four fields in the IDL's order.
fn cb_device_to_json(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), DEVICE) else { return };
    let out = v8::Object::new(scope);
    for k in ["deviceId", "kind", "label", "groupId"] {
        let v = st::get(scope, s, k);
        st::set(scope, out, k, v);
    }
    rv.set(out.into());
}

/// `InputDeviceInfo.getCapabilities()`: "If label is null, permissions have not been given and no
/// capabilities should be returned" (`InputDeviceInfo::getCapabilities`) -- an empty dictionary, which is every
/// device this engine lists, since it lists them unprompted.
fn cb_input_capabilities(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // The receiver must be an INPUT device: an `audiooutput` entry reached through `call` is as foreign to
    // `InputDeviceInfo`'s binding as a plain object.
    let kind = st::state(scope, args.this().into(), DEVICE).map(|s| st::get_str(scope, s, "kind"));
    if !matches!(kind.as_deref(), Some("audioinput" | "videoinput")) {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return;
    }
    let o = v8::Object::new(scope);
    rv.set(o.into());
}

pub fn install_media_device_info(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "deviceId" | "kind" | "label" | "groupId" => acc_get(scope, proto, member, cb_device_field, member.name),
        "toJSON" => method_row(scope, proto, member, cb_device_to_json),
        other => debug_assert!(false, "install_media_device_info: unexpected member {other}"),
    }
}

pub fn install_input_device_info(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "getCapabilities" => method_row(scope, proto, member, cb_input_capabilities),
        other => debug_assert!(false, "install_input_device_info: unexpected member {other}"),
    }
}

// ---- the tables ---------------------------------------------------------------------------------

webapi::interface! {
    "MediaDeviceInfo" [interface, window, secure] {
    ctor   "MediaDeviceInfo" len 0               = stub,
    attr   "deviceId" : DomString readonly        = install_media_device_info,
    attr   "kind" : Any readonly                  = install_media_device_info,
    attr   "label" : DomString readonly           = install_media_device_info,
    attr   "groupId" : DomString readonly         = install_media_device_info,
    method "toJSON" : Any len 0                   = install_media_device_info
    }
}

webapi::interface! {
    "InputDeviceInfo" : "MediaDeviceInfo" [interface, window] {
    ctor   "InputDeviceInfo" len 0                = stub,
    method "getCapabilities" : Any len 0          = install_input_device_info
    }
}
