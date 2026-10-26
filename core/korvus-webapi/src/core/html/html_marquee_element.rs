//! `core/html/html_marquee_element.rs` -- `HTMLMarqueeElement` as an `interface!` table, for the one
//! row the generated registry could not express: `loop` (the Chrome oracle).
//!
//! `loop` is a `long` with Blink's own rules (`HTMLMarqueeElement::loop` / `setLoop`): the getter parses
//! the `loop` attribute as an HTML integer and answers -1 when it is missing, unparsable or not
//! positive; the setter refuses a value that is neither positive nor -1 with an IndexSizeError and
//! otherwise writes the integer. The registry row was a D2 stub answering `undefined`, and
//! `loop = "1"` read back the string "1" (Chrome 150: `-1`, `1`, and the IndexSizeError for 0).
//!
//! The other rows are the registry's VERBATIM (order, kinds, `cfg`s); `start()` / `stop()` stay stubs:
//! there is no marquee animation to start or stop.

use deno_core::v8;

use crate::install::MemberDef;

fn cb_marquee_loop_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = crate::page::node_id(scope, args.this().into()) else { return };
    let raw = crate::page::with_host_dom(scope, |d| d.get_attribute(id, "loop").map(str::to_string)).flatten();
    let n = raw
        .and_then(|r| crate::core::dom::element::parse_html_integer(&r, true))
        .filter(|n| *n > 0 && *n <= i32::MAX as i64)
        .unwrap_or(-1);
    rv.set_int32(n as i32);
}

fn cb_marquee_loop_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    // WebIDL `long`: ToNumber, then ToInt32.
    let Some(n) = args.get(0).int32_value(scope) else { return };
    if n <= 0 && n != -1 {
        crate::binding::protos::throw_dom(scope, &format!("Failed to set the 'loop' property on 'HTMLMarqueeElement': The provided value ({n}) is neither positive nor -1."), "IndexSizeError");
        return;
    }
    let el: v8::Local<v8::Value> = args.this().into();
    let name = crate::binding::protos::str_val(scope, "loop");
    let val = crate::binding::protos::str_val(scope, &n.to_string());
    crate::core::dom::element::attr_call(scope, el, "setAttribute", &[name, val]);
}

pub fn install_html_marquee_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "loop" => crate::install::live::pair_row(scope, proto, member, cb_marquee_loop_get, cb_marquee_loop_set),
        other => debug_assert!(false, "install_html_marquee_element: unexpected member {other}"),
    }
}

webapi::interface! {
    "HTMLMarqueeElement" : "HTMLElement" [element, window, backfill] {
        ctor   "HTMLMarqueeElement"            len 0 = stub,
        attr   "behavior"     : DomString cfg "behavior"          = reflect_str,
        attr   "bgColor"      : DomString cfg "bgcolor"           = reflect_str,
        attr   "direction"    : DomString cfg "direction"         = reflect_str,
        attr   "height"       : DomString cfg "height"            = reflect_str,
        attr   "hspace"       : Integer   cfg "hspace|0|"         = reflect_ulong,
        attr   "loop"         : Integer                           = install_html_marquee_element,
        attr   "scrollAmount" : Integer   cfg "scrollamount|6|"   = reflect_ulong,
        attr   "scrollDelay"  : Integer   cfg "scrolldelay|85|"   = reflect_ulong,
        attr   "trueSpeed"    : Boolean   cfg "truespeed"         = reflect_bool,
        attr   "vspace"       : Integer   cfg "vspace|0|"         = reflect_ulong,
        attr   "width"        : DomString cfg "width"             = reflect_str,
        method "start"        : Undefined len 0                   = stub,
        method "stop"         : Undefined len 0                   = stub
    }
}
