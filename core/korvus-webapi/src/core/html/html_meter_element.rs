//! `core/html/html_meter_element.rs` -- `HTMLMeterElement` as one `interface!` table.
//!
//! `value`/`min`/`max`/`low`/`high`/`optimum` are NOT the generic string reflection: the IDL type is
//! `double` and only the SETTER is a bare `[ReflectSetter]`. The GETTER is the element's own
//! clamp-against-its-siblings algorithm -- a `<meter>` with no attributes reports
//! `min 0, max 1, value 0, low 0, high 1, optimum 0.5`, each derived from the others. There is no
//! constructor: `new HTMLMeterElement()` throws (the `stub` ctor row is the Illegal-constructor answer).
//!
//! The bodies were already Rust before this lane (the glue-body phase moved them out of
//! `dom/08m-meter-element.js`), reached through `op_install_meter` -- a hand install that the one-line
//! JS module `dom/08m-meter-element.js` called at its bootstrap position. W5.1 L13 converts that hand
//! install into the seven rows below, naming the SAME callbacks; the op, the module and the
//! `HTMLMeterElement` entry of `dom/08b-reflected.js`'s `op_control_value_get` loop (dead since the
//! glue-body phase, because 08m ran after it and overwrote `value`) are deleted with it.
//!
//! `labels` reuses the native scan written for `HTMLInputElement` -- every labelable element shares it.
//!
//! RECEIVER CHECKS. The seven bodies reject a receiver that is not a `<meter>` with Chrome's
//! `TypeError: Illegal invocation` (measured in Chrome 151 over loopback). Before this lane
//! `HTMLMeterElement.prototype.__lookupGetter__("value").call({})` answered `0`, because the clamp
//! algorithm asks the receiver for its attributes and a plain object has none.

use deno_core::v8;

use crate::binding::protos::{str_val, throw_type_error};
use crate::core::dom::element::el_id_of;
use crate::core::html::html_input_element::cb_input_labels_get;
use crate::install::MemberDef;

/// "Rules for parsing floating-point number values" (HTML std): a lenient prefix parse from the start of
/// the whitespace-trimmed string. Trailing garbage is fine (`"5abc"` -> 5), a leading `+` fails (only `-`
/// is a recognised sign), at least one digit is required. `None` on failure -- distinct from a parsed 0.
/// A faithful port of dom/08m-meter-element.js `meterParseFloat` (Rust `Display` is always decimal, so a
/// value produced by the setter round-trips back through this).
fn meter_parse_float(s: &str) -> Option<f64> {
    let c: Vec<char> = s.chars().collect();
    let n = c.len();
    let mut i = 0;
    let is_ws = |ch: char| matches!(ch, ' ' | '\t' | '\n' | '\u{000c}' | '\r');
    while i < n && is_ws(c[i]) {
        i += 1;
    }
    let mut out = String::new();
    if i < n && c[i] == '-' {
        out.push('-');
        i += 1;
    }
    while i < n && c[i].is_ascii_digit() {
        out.push(c[i]);
        i += 1;
    }
    if i < n && c[i] == '.' && i + 1 < n && c[i + 1].is_ascii_digit() {
        out.push('.');
        i += 1;
        while i < n && c[i].is_ascii_digit() {
            out.push(c[i]);
            i += 1;
        }
    }
    if !out.chars().any(|ch| ch.is_ascii_digit()) {
        return None;
    }
    if i < n && (c[i] == 'e' || c[i] == 'E') {
        let mut j = i + 1;
        let mut sign = String::new();
        if j < n && (c[j] == '+' || c[j] == '-') {
            sign.push(c[j]);
            j += 1;
        }
        if j < n && c[j].is_ascii_digit() {
            let mut exp = String::new();
            while j < n && c[j].is_ascii_digit() {
                exp.push(c[j]);
                j += 1;
            }
            out.push('e');
            out.push_str(&sign);
            out.push_str(&exp);
        }
    }
    match out.parse::<f64>() {
        Ok(v) if !v.is_nan() => Some(v),
        _ => None,
    }
}

/// "Get the floating point value of `attr`, or `fallback`" (Blink's GetFloatingPointAttribute). An absent
/// attribute and an unparseable one both fall back. Reads through the element's own `hasAttribute`/
/// `getAttribute` (the same path the reflect accessors use), so it stays live across `setAttribute`.
fn meter_attr_or<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Value>, attr: &str, fallback: f64) -> f64 {
    let a = str_val(scope, attr);
    let has = crate::core::dom::element::attr_call(scope, this, "hasAttribute", &[a]).map(|v| v.boolean_value(scope)).unwrap_or(false);
    if !has {
        return fallback;
    }
    let a = str_val(scope, attr);
    match crate::core::dom::element::attr_call(scope, this, "getAttribute", &[a]) {
        Some(v) if !v.is_null_or_undefined() => meter_parse_float(&v.to_rust_string_lossy(scope)).unwrap_or(fallback),
        _ => fallback,
    }
}

struct MeterState {
    value: f64,
    min: f64,
    max: f64,
    low: f64,
    high: f64,
    optimum: f64,
}

/// The whole gauge resolved at once, in the order the spec's clamps depend on each other: `max` cannot
/// fall below `min`; `value`/`low`/`optimum` pin into `[min, max]`; `high` into `[low, max]`; `optimum`
/// defaults to the midpoint. A faithful port of `KB.__meterState`.
fn meter_state<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Value>) -> MeterState {
    let min = meter_attr_or(scope, this, "min", 0.0);
    let max = min.max(meter_attr_or(scope, this, "max", 1.0));
    let value = meter_attr_or(scope, this, "value", 0.0).max(min).min(max);
    let low = meter_attr_or(scope, this, "low", min).max(min).min(max);
    let high = meter_attr_or(scope, this, "high", max).min(max).max(low);
    let optimum = meter_attr_or(scope, this, "optimum", min + (max - min) / 2.0).max(min).min(max);
    MeterState { value, min, max, low, high, optimum }
}

/// `true` when the receiver is a `<meter>`; otherwise Chrome's `Illegal invocation` TypeError.
fn this_meter(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> bool {
    let this = args.this();
    let ok = crate::page::tag_name_of(scope, (this).into()).is_some_and(|t| t.eq_ignore_ascii_case("meter"))
        && el_id_of(scope, this).is_some();
    if !ok {
        throw_type_error(scope, "Illegal invocation");
    }
    ok
}

macro_rules! meter_getter {
    ($name:ident, $field:ident) => {
        fn $name(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
            if !this_meter(scope, &args) {
                return;
            }
            let st = meter_state(scope, args.this().into());
            rv.set(v8::Number::new(scope, st.$field).into());
        }
    };
}
meter_getter!(cb_meter_value_get, value);
meter_getter!(cb_meter_min_get, min);
meter_getter!(cb_meter_max_get, max);
meter_getter!(cb_meter_low_get, low);
meter_getter!(cb_meter_high_get, high);
meter_getter!(cb_meter_optimum_get, optimum);

/// The shared `[ReflectSetter]` body (the attribute name rides on the accessor's data). WebIDL `double`
/// conversion runs first, so a non-finite value throws `TypeError` and never reaches the attribute; on
/// success the attribute takes the number's string form. A faithful port of `KB.__meter*Set`.
fn cb_meter_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !this_meter(scope, &args) {
        return;
    }
    let attr = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let n = args.get(0).number_value(scope).unwrap_or(f64::NAN);
    if !n.is_finite() {
        throw_type_error(
            scope,
            &format!("Failed to set the '{attr}' property on 'HTMLMeterElement': The provided double value is non-finite."),
        );
        return;
    }
    let a = str_val(scope, &attr);
    let v = str_val(scope, &format!("{n}"));
    crate::core::dom::element::attr_call(scope, args.this().into(), "setAttribute", &[a, v]);
}

/// Mount one row: the six `double` properties are the native clamp getter + the shared `[ReflectSetter]`
/// (the attribute name rides on the setter's data); `labels` is `<input>`'s own native scan.
pub fn install_html_meter_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::core::html::html_input_element::{acc1, acc2};
    match member.name {
        "value" => acc2(scope, proto, member, cb_meter_value_get, None, cb_meter_set, Some("value")),
        "min" => acc2(scope, proto, member, cb_meter_min_get, None, cb_meter_set, Some("min")),
        "max" => acc2(scope, proto, member, cb_meter_max_get, None, cb_meter_set, Some("max")),
        "low" => acc2(scope, proto, member, cb_meter_low_get, None, cb_meter_set, Some("low")),
        "high" => acc2(scope, proto, member, cb_meter_high_get, None, cb_meter_set, Some("high")),
        "optimum" => acc2(scope, proto, member, cb_meter_optimum_get, None, cb_meter_set, Some("optimum")),
        "labels" => acc1(scope, proto, member, cb_input_labels_get, None),
        other => debug_assert!(false, "install_html_meter_element: unexpected member {other}"),
    }
}

webapi::interface! {
    "HTMLMeterElement" : "HTMLElement" [element, window] {
        ctor   "HTMLMeterElement"           len 0    = stub,
        attr   "value"   : Double           = install_html_meter_element,
        attr   "min"     : Double           = install_html_meter_element,
        attr   "max"     : Double           = install_html_meter_element,
        attr   "low"     : Double           = install_html_meter_element,
        attr   "high"    : Double           = install_html_meter_element,
        attr   "optimum" : Double           = install_html_meter_element,
        attr   "labels"  : Object  readonly = install_html_meter_element
    }
}
