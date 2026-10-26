//! `core/webrtc/ice_event.rs` -- `RTCPeerConnectionIceEvent`, the `icecandidate` event's interface.
//!
//! It was a generated registry SHELL -- constructor and `candidate` both stubs -- so gathering fired a plain
//! `Event` with an OWN `candidate` property: `e instanceof RTCPeerConnectionIceEvent` was false,
//! `Object.getOwnPropertyNames(e)` was `["isTrusted", "candidate"]`, and a page reading `e.candidate`
//! never reached the interface's getter. Chrome 150.0.7871.46, measured: the event Chrome fires is an
//! `RTCPeerConnectionIceEvent` whose only own property is `isTrusted`; `new RTCPeerConnectionIceEvent(t,
//! {candidate: c}).candidate === c`, a missing or null `candidate` reads null, the prototype's own keys
//! are `candidate` and `constructor`, and the constructor's `length` is 1.

use deno_core::v8;

use crate::core::events::ctor::{arg_string, brand, init_dict, init_event, require_args, require_new};
use crate::core::events::event::{evt_slot_write, evt_slot_write_null};
use crate::install::installer::InstallLedger;

pub(crate) fn cb_ice_event_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    const IFACE: &str = "RTCPeerConnectionIceEvent";
    if !require_new(scope, &args, IFACE) || !require_args(scope, &args, IFACE, 1) {
        return;
    }
    let Some(ty) = arg_string(scope, &args, 0, IFACE) else { return };
    let Ok(dict) = init_dict(scope, &args, 1, IFACE, "RTCPeerConnectionIceEventInit") else { return };
    let this = args.this();
    init_event(scope, this, &ty, dict);
    // `RTCIceCandidate? candidate = null`
    let cand = match dict {
        Some(d) => {
            let k = crate::binding::protos::str_val(scope, "candidate");
            match d.get(scope, k) {
                Some(v) => v,
                None => return,
            }
        }
        None => v8::undefined(scope).into(),
    };
    if cand.is_null_or_undefined() {
        evt_slot_write_null(scope, this, "candidate");
    } else if crate::install::webidl::is_instance_of(scope, cand, "RTCIceCandidate") {
        evt_slot_write(scope, this, "candidate", cand);
    } else {
        crate::binding::protos::throw_type_error(
            scope,
            "Failed to construct 'RTCPeerConnectionIceEvent': Failed to read the 'candidate' property from 'RTCPeerConnectionIceEventInit': Failed to convert value to 'RTCIceCandidate'.",
        );
        return;
    }
    brand(scope, this, IFACE);
    rv.set(this.into());
}

/// Created at its capture position by the registry walk (`live::ORDERED_HAND`).
pub(crate) fn create_rtc_peer_connection_ice_event<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, ledger: &mut InstallLedger) {
    crate::install::live::rows::build_newable(scope, global, &RTCPEERCONNECTIONICEEVENT, cb_ice_event_ctor, Some("Event"), None, ledger);
}

webapi::interface! {
    "RTCPeerConnectionIceEvent" : "Event" [value, window] {
        ctor "RTCPeerConnectionIceEvent"                   len 1 = stub,
        attr "candidate" : Any readonly nullable                 = evt_get_null
    }
}
