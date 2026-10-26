//! `core/css/typed_om.rs` -- the CSS Typed OM's values (the Chrome oracle
//! ratchet): `CSSUnitValue`, `CSSKeywordValue`, the `CSSMath*` tree, `CSSNumericArray`, the transform
//! components and `CSSTransformValue`, `CSSUnparsedValue` / `CSSVariableReferenceValue`, the `CSS.px()`
//! family of factories, `CSS.registerProperty`, `CSS.paintWorklet`, and `StylePropertyMap`'s writes.
//!
//! Every interface here was a generated stub: `new CSSUnitValue(1, "px").value` was `undefined`,
//! `CSS.px` did not exist (Chrome's `CSS` namespace has 66 unit factories), `new CSSMathSum()` built an
//! object where Chrome throws SyntaxError. The algorithms are Blink's (`core/css/cssom/*.cc`, Chromium
//! 150), and every answer below is measured on stock Chrome 150
//! (`core/korvus-runtime/tests/fixtures/typed_om/`):
//!
//! - a unit value serializes with `%.6g` (`CSS.px(1234567)` is `1.23457e+06px`), `percent` as `%`,
//!   `number` bare; unit names are case-insensitive and `%` is `percent`;
//! - arithmetic (`add` / `sub` / `mul` / `div` / `min` / `max`) folds to a unit value when every operand
//!   has the same unit (one non-number for `mul`), else builds the math node, flattening an operand of
//!   the same kind; types add / multiply with the percent hint (`CSSNumericValueType`), "Incompatible
//!   types" otherwise;
//! - `to()` / `toSum()` go through the sum value with canonical units (px, deg, s, hz, dppx);
//! - `CSSNumericValue.parse` takes one numeric token or a `calc()` / `min()` / `max()` / `clamp()`,
//!   simplified as Blink's math-expression parser does (like terms combine at their first position,
//!   a number distributes over a sum) and converted as `CalcToNumericValue` does;
//! - transform components validate their arguments' types and build their `DOMMatrix` through the
//!   engine's own `DOMMatrix` methods;
//! - `CSSUnparsedValue` serializes through the CSS tokenizer, inserting `/**/` only between tokens the
//!   CSS Syntax serialization table says would merge (`a/**/b`, `var(--a)var(--b)`).
//!
//! TWO RESIDUALS, stated. (1) An indexed property of `CSSTransformValue` / `CSSUnparsedValue` is a plain
//! writable own property: Chrome's indexed SETTER type-checks and refuses a gap (RangeError); this one
//! stores what it is given. (2) `CSSPerspective.toMatrix()` is a 3D matrix here; Blink's
//! `perspectiveSelf` leaves the result flagged 2D, so Chrome prints `matrix(1, 0, 0, 1, 0, 0)`.

use deno_core::v8;

use crate::binding::protos::{body_data, build_fn_data, call_method, get_prop, str_val, throw_dom, throw_range_error, throw_type_error};
use crate::install::MemberDef;

// ── private slots ───────────────────────────────────────────────────────────────────────────────

fn key<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, &format!("korvus_tom_{name}")).unwrap();
    v8::Private::for_api(scope, Some(n))
}
fn pset(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, name: &str, v: v8::Local<v8::Value>) {
    let k = key(scope, name);
    o.set_private(scope, k, v);
}
fn pget<'s>(scope: &mut v8::PinScope<'s, '_>, o: v8::Local<v8::Object>, name: &str) -> Option<v8::Local<'s, v8::Value>> {
    let k = key(scope, name);
    o.get_private(scope, k).filter(|v| !v.is_undefined())
}
fn pobj<'s>(scope: &mut v8::PinScope<'s, '_>, o: v8::Local<v8::Object>, name: &str) -> Option<v8::Local<'s, v8::Object>> {
    pget(scope, o, name).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
}
fn pstr(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, name: &str) -> Option<String> {
    pget(scope, o, name).map(|v| v.to_rust_string_lossy(scope))
}
fn pnum(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, name: &str) -> Option<f64> {
    pget(scope, o, name).and_then(|v| v.number_value(scope))
}
fn pset_str(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, name: &str, s: &str) {
    let v = str_val(scope, s);
    pset(scope, o, name, v);
}
fn pset_num(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, name: &str, n: f64) {
    let v = v8::Number::new(scope, n).into();
    pset(scope, o, name, v);
}
fn pset_bool(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, name: &str, b: bool) {
    let v = v8::Boolean::new(scope, b).into();
    pset(scope, o, name, v);
}

fn mint<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str) -> Option<v8::Local<'s, v8::Object>> {
    let ctor = crate::binding::protos::iface_object(scope, iface).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let proto = get_prop(scope, ctor, "prototype").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let o = crate::binding::protos::mint_on(scope, proto.into());
    crate::binding::protos::brand_shell_instance(scope, o);
    crate::install::webidl::brand(scope, o, iface);
    Some(o)
}

// ── units ─────────────────────────────────────────────────────────────────────────────────────────

const LENGTH: usize = 0;
const ANGLE: usize = 1;
const TIME: usize = 2;
const FREQUENCY: usize = 3;
const RESOLUTION: usize = 4;
const FLEX: usize = 5;
const PERCENT: usize = 6;
const BASE_NAMES: [&str; 7] = ["length", "angle", "time", "frequency", "resolution", "flex", "percent"];

/// `(name, base type, factor to the canonical unit, canonical unit)`; `None` base is `number`.
/// Relative lengths have no canonical unit (Blink: category kUOther), so they convert only to themselves.
const UNITS: &[(&str, Option<usize>, f64, Option<&str>)] = &[
    ("number", None, 1.0, Some("number")),
    ("percent", Some(PERCENT), 1.0, None),
    ("em", Some(LENGTH), 1.0, None),
    ("rem", Some(LENGTH), 1.0, None),
    ("ex", Some(LENGTH), 1.0, None),
    ("rex", Some(LENGTH), 1.0, None),
    ("ch", Some(LENGTH), 1.0, None),
    ("rch", Some(LENGTH), 1.0, None),
    ("ic", Some(LENGTH), 1.0, None),
    ("ric", Some(LENGTH), 1.0, None),
    ("lh", Some(LENGTH), 1.0, None),
    ("rlh", Some(LENGTH), 1.0, None),
    ("cap", Some(LENGTH), 1.0, None),
    ("rcap", Some(LENGTH), 1.0, None),
    ("vw", Some(LENGTH), 1.0, None),
    ("vh", Some(LENGTH), 1.0, None),
    ("vi", Some(LENGTH), 1.0, None),
    ("vb", Some(LENGTH), 1.0, None),
    ("vmin", Some(LENGTH), 1.0, None),
    ("vmax", Some(LENGTH), 1.0, None),
    ("svw", Some(LENGTH), 1.0, None),
    ("svh", Some(LENGTH), 1.0, None),
    ("svi", Some(LENGTH), 1.0, None),
    ("svb", Some(LENGTH), 1.0, None),
    ("svmin", Some(LENGTH), 1.0, None),
    ("svmax", Some(LENGTH), 1.0, None),
    ("lvw", Some(LENGTH), 1.0, None),
    ("lvh", Some(LENGTH), 1.0, None),
    ("lvi", Some(LENGTH), 1.0, None),
    ("lvb", Some(LENGTH), 1.0, None),
    ("lvmin", Some(LENGTH), 1.0, None),
    ("lvmax", Some(LENGTH), 1.0, None),
    ("dvw", Some(LENGTH), 1.0, None),
    ("dvh", Some(LENGTH), 1.0, None),
    ("dvi", Some(LENGTH), 1.0, None),
    ("dvb", Some(LENGTH), 1.0, None),
    ("dvmin", Some(LENGTH), 1.0, None),
    ("dvmax", Some(LENGTH), 1.0, None),
    ("cqw", Some(LENGTH), 1.0, None),
    ("cqh", Some(LENGTH), 1.0, None),
    ("cqi", Some(LENGTH), 1.0, None),
    ("cqb", Some(LENGTH), 1.0, None),
    ("cqmin", Some(LENGTH), 1.0, None),
    ("cqmax", Some(LENGTH), 1.0, None),
    ("cm", Some(LENGTH), 96.0 / 2.54, Some("px")),
    ("mm", Some(LENGTH), 96.0 / 25.4, Some("px")),
    ("in", Some(LENGTH), 96.0, Some("px")),
    ("pt", Some(LENGTH), 96.0 / 72.0, Some("px")),
    ("pc", Some(LENGTH), 16.0, Some("px")),
    ("px", Some(LENGTH), 1.0, Some("px")),
    ("q", Some(LENGTH), 96.0 / 101.6, Some("px")),
    ("deg", Some(ANGLE), 1.0, Some("deg")),
    ("grad", Some(ANGLE), 0.9, Some("deg")),
    ("rad", Some(ANGLE), 180.0 / std::f64::consts::PI, Some("deg")),
    ("turn", Some(ANGLE), 360.0, Some("deg")),
    ("s", Some(TIME), 1.0, Some("s")),
    ("ms", Some(TIME), 0.001, Some("s")),
    ("hz", Some(FREQUENCY), 1.0, Some("hz")),
    ("khz", Some(FREQUENCY), 1000.0, Some("hz")),
    ("dpi", Some(RESOLUTION), 1.0 / 96.0, Some("dppx")),
    ("dpcm", Some(RESOLUTION), 2.54 / 96.0, Some("dppx")),
    ("dppx", Some(RESOLUTION), 1.0, Some("dppx")),
    ("x", Some(RESOLUTION), 1.0, Some("dppx")),
    ("fr", Some(FLEX), 1.0, None),
];

/// The `CSS` namespace's factories, in Blink's `css_unit_values.idl` spelling (`Hz`, `Q`, ...).
pub(crate) const FACTORIES: &[&str] = &[
    "number", "percent", "em", "rem", "ex", "rex", "ch", "rch", "ic", "ric", "lh", "rlh", "cap", "rcap", "vw", "vh", "vi", "vb", "vmin",
    "vmax", "svw", "svh", "svi", "svb", "svmin", "svmax", "lvw", "lvh", "lvi", "lvb", "lvmin", "lvmax", "dvw", "dvh", "dvi", "dvb",
    "dvmin", "dvmax", "cqw", "cqh", "cqi", "cqb", "cqmin", "cqmax", "cm", "mm", "in", "pt", "pc", "px", "Q", "deg", "grad", "rad", "turn",
    "s", "ms", "Hz", "kHz", "dpi", "dpcm", "dppx", "x", "fr",
];

/// Blink's `CSSNumericValue::UnitFromName` restricted to `IsValidUnit`: the canonical name, or `None`.
fn unit_from_name(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return None;
    }
    let lower = name.to_ascii_lowercase();
    if lower == "%" {
        return Some("percent");
    }
    UNITS.iter().find(|u| u.0 == lower).map(|u| u.0)
}
fn unit_info(unit: &str) -> (&'static str, Option<usize>, f64, Option<&'static str>) {
    UNITS.iter().find(|u| u.0 == unit).copied().unwrap_or(("number", None, 1.0, Some("number")))
}

/// `%.6g`, as Blink's `FormatNumber` prints a CSS number.
pub(crate) fn fmt_number(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "infinity".into() } else { "-infinity".into() };
    }
    if x == 0.0 {
        return "0".into();
    }
    let e = format!("{x:.5e}");
    let (mant, exp) = e.split_once('e').unwrap_or((&e, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if !(-4..6).contains(&exp) {
        let mut m = mant.to_string();
        if m.contains('.') {
            m = m.trim_end_matches('0').trim_end_matches('.').to_string();
        }
        return format!("{m}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs());
    }
    let decimals = (5 - exp).max(0) as usize;
    let mut s = format!("{x:.decimals$}");
    if s.contains('.') {
        s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    if s == "-0" {
        s = "0".into();
    }
    s
}

fn unit_suffix(unit: &str) -> &str {
    match unit {
        "number" => "",
        "percent" => "%",
        u => u,
    }
}

// ── the numeric type (Blink `CSSNumericValueType`) ────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Debug)]
struct NType {
    e: [i32; 7],
    hint: Option<usize>,
}
impl NType {
    fn of_unit(unit: &str) -> NType {
        let mut e = [0; 7];
        if let Some(b) = unit_info(unit).1 {
            e[b] = 1;
        }
        NType { e, hint: None }
    }
    fn apply_hint(&mut self, hint: usize) {
        self.e[hint] += self.e[PERCENT];
        self.e[PERCENT] = 0;
        self.hint = Some(hint);
    }
    fn add(mut a: NType, mut b: NType) -> Option<NType> {
        if let (Some(x), Some(y)) = (a.hint, b.hint) {
            if x != y {
                return None;
            }
        }
        if let Some(h) = a.hint {
            b.apply_hint(h);
        } else if let Some(h) = b.hint {
            a.apply_hint(h);
        }
        for i in 0..7 {
            if a.e[i] != b.e[i] {
                if i != PERCENT {
                    a.apply_hint(i);
                    b.apply_hint(i);
                }
                if a.e[i] != b.e[i] {
                    return None;
                }
            }
        }
        Some(a)
    }
    fn mul(mut a: NType, mut b: NType) -> Option<NType> {
        if let (Some(x), Some(y)) = (a.hint, b.hint) {
            if x != y {
                return None;
            }
        }
        if let Some(h) = a.hint {
            b.apply_hint(h);
        } else if let Some(h) = b.hint {
            a.apply_hint(h);
        }
        for i in 0..7 {
            a.e[i] += b.e[i];
        }
        Some(a)
    }
    fn negate(mut self) -> NType {
        for v in self.e.iter_mut() {
            *v = -*v;
        }
        self
    }
    /// A `<length-percentage>`-compatible type: length 1 (a percent folded in by the hint) or percent 1.
    fn is_length_or_percent(&self) -> bool {
        let only = |i: usize| self.e.iter().enumerate().all(|(j, v)| if j == i { *v == 1 } else { *v == 0 });
        only(LENGTH) || (only(PERCENT) && self.hint.is_none())
    }
    fn is_only(&self, i: usize) -> bool {
        self.hint.is_none() && self.e.iter().enumerate().all(|(j, v)| if j == i { *v == 1 } else { *v == 0 })
    }
    fn is_number(&self) -> bool {
        self.e.iter().all(|v| *v == 0) && self.hint.is_none()
    }
}

// ── numeric value objects ─────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Unit,
    Sum,
    Product,
    Negate,
    Invert,
    Min,
    Max,
    Clamp,
}
impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Unit => "unit",
            Kind::Sum => "sum",
            Kind::Product => "product",
            Kind::Negate => "negate",
            Kind::Invert => "invert",
            Kind::Min => "min",
            Kind::Max => "max",
            Kind::Clamp => "clamp",
        }
    }
    fn iface(self) -> &'static str {
        match self {
            Kind::Unit => "CSSUnitValue",
            Kind::Sum => "CSSMathSum",
            Kind::Product => "CSSMathProduct",
            Kind::Negate => "CSSMathNegate",
            Kind::Invert => "CSSMathInvert",
            Kind::Min => "CSSMathMin",
            Kind::Max => "CSSMathMax",
            Kind::Clamp => "CSSMathClamp",
        }
    }
    fn from_name(s: &str) -> Option<Kind> {
        Some(match s {
            "unit" => Kind::Unit,
            "sum" => Kind::Sum,
            "product" => Kind::Product,
            "negate" => Kind::Negate,
            "invert" => Kind::Invert,
            "min" => Kind::Min,
            "max" => Kind::Max,
            "clamp" => Kind::Clamp,
            _ => return None,
        })
    }
    fn variadic(self) -> bool {
        matches!(self, Kind::Sum | Kind::Product | Kind::Min | Kind::Max)
    }
}

/// The numeric value `v` is, if it is one: its kind and the object.
fn numeric<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'s, v8::Value>) -> Option<(Kind, v8::Local<'s, v8::Object>)> {
    let o = v8::Local::<v8::Object>::try_from(v).ok()?;
    let k = pstr(scope, o, "kind")?;
    Some((Kind::from_name(&k)?, o))
}
fn kind(scope: &mut v8::PinScope, o: v8::Local<v8::Object>) -> Option<Kind> {
    pstr(scope, o, "kind").and_then(|k| Kind::from_name(&k))
}
fn unit_parts(scope: &mut v8::PinScope, o: v8::Local<v8::Object>) -> (f64, String) {
    (pnum(scope, o, "v").unwrap_or(0.0), pstr(scope, o, "u").unwrap_or_else(|| "number".into()))
}
/// A math value's operands, in order (the variadic's array, a unary's value, clamp's three).
fn children<'s>(scope: &mut v8::PinScope<'s, '_>, o: v8::Local<v8::Object>) -> Vec<v8::Local<'s, v8::Object>> {
    match kind(scope, o) {
        Some(k) if k.variadic() => {
            let Some(arr) = pobj(scope, o, "vals") else { return Vec::new() };
            let Some(items) = pget(scope, arr, "items").and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) else { return Vec::new() };
            (0..items.length()).filter_map(|i| items.get_index(scope, i)).filter_map(|v| v8::Local::<v8::Object>::try_from(v).ok()).collect()
        }
        Some(Kind::Negate) | Some(Kind::Invert) => pobj(scope, o, "val").into_iter().collect(),
        Some(Kind::Clamp) => ["lower", "val", "upper"].iter().filter_map(|k| pobj(scope, o, k)).collect(),
        _ => Vec::new(),
    }
}

fn ntype(scope: &mut v8::PinScope, o: v8::Local<v8::Object>) -> Option<NType> {
    let k = kind(scope, o)?;
    match k {
        Kind::Unit => {
            let (_, u) = unit_parts(scope, o);
            Some(NType::of_unit(&u))
        }
        Kind::Negate => {
            let c = children(scope, o);
            ntype(scope, *c.first()?)
        }
        Kind::Invert => {
            let c = children(scope, o);
            Some(ntype(scope, *c.first()?)?.negate())
        }
        Kind::Product => {
            let c = children(scope, o);
            type_check(scope, &c, NType::mul)
        }
        _ => {
            let c = children(scope, o);
            type_check(scope, &c, NType::add)
        }
    }
}
fn type_check(scope: &mut v8::PinScope, items: &[v8::Local<v8::Object>], op: fn(NType, NType) -> Option<NType>) -> Option<NType> {
    let mut t = ntype(scope, *items.first()?)?;
    for o in &items[1..] {
        let n = ntype(scope, *o)?;
        t = op(t, n)?;
    }
    Some(t)
}

fn new_unit<'s>(scope: &mut v8::PinScope<'s, '_>, v: f64, unit: &str) -> Option<v8::Local<'s, v8::Object>> {
    let o = mint(scope, "CSSUnitValue")?;
    init_unit(scope, o, v, unit);
    Some(o)
}
fn init_unit(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, v: f64, unit: &str) {
    pset_str(scope, o, "kind", "unit");
    pset_num(scope, o, "v", v);
    pset_str(scope, o, "u", unit);
}

fn define_index(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, i: u32, v: v8::Local<v8::Value>, writable: bool) {
    let k: v8::Local<v8::Name> = v8::String::new(scope, &i.to_string()).unwrap().into();
    let attr = if writable { v8::PropertyAttribute::NONE } else { v8::PropertyAttribute::READ_ONLY };
    let _ = o.define_own_property(scope, k, v, attr);
}

fn init_math(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, k: Kind, items: &[v8::Local<v8::Object>]) {
    pset_str(scope, o, "kind", k.name());
    match k {
        Kind::Negate | Kind::Invert => pset(scope, o, "val", items[0].into()),
        Kind::Clamp => {
            pset(scope, o, "lower", items[0].into());
            pset(scope, o, "val", items[1].into());
            pset(scope, o, "upper", items[2].into());
        }
        _ => {
            // A `CSSNumericArray` over the operands: indexed, read-only, `[SameObject]`.
            let vals: Vec<v8::Local<v8::Value>> = items.iter().map(|o| (*o).into()).collect();
            if let Some(arr) = mint(scope, "CSSNumericArray") {
                for (i, v) in vals.iter().enumerate() {
                    define_index(scope, arr, i as u32, *v, false);
                }
                let a = v8::Array::new_with_elements(scope, &vals);
                pset(scope, arr, "items", a.into());
                pset(scope, o, "vals", arr.into());
            }
        }
    }
}
fn new_math<'s>(scope: &mut v8::PinScope<'s, '_>, k: Kind, items: &[v8::Local<'s, v8::Object>]) -> Option<v8::Local<'s, v8::Object>> {
    let o = mint(scope, k.iface())?;
    init_math(scope, o, k, items);
    Some(o)
}
/// `CSSMathSum::Create` and its siblings: the type check, then the value -- "Incompatible types".
fn create_math<'s>(scope: &mut v8::PinScope<'s, '_>, k: Kind, items: &[v8::Local<'s, v8::Object>], ctx: &str) -> Option<v8::Local<'s, v8::Object>> {
    let op = if k == Kind::Product { NType::mul } else { NType::add };
    if type_check(scope, items, op).is_none() {
        throw_type_error(scope, &format!("{ctx}Incompatible types"));
        return None;
    }
    new_math(scope, k, items)
}

/// `CSSNumberish` -> `CSSNumericValue`: a numeric value as it is, anything else a `double` (a TypeError
/// when non-finite) made a `number` unit value. `None` = an exception is pending.
fn numberish<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'s, v8::Value>, ctx: &str) -> Option<v8::Local<'s, v8::Object>> {
    if let Some((_, o)) = numeric(scope, v) {
        return Some(o);
    }
    let n = v.number_value(scope)?;
    if !n.is_finite() {
        throw_type_error(scope, &format!("{ctx}The provided double value is non-finite."));
        return None;
    }
    new_unit(scope, n, "number")
}
fn numberishes<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments<'s>, from: i32, ctx: &str) -> Option<Vec<v8::Local<'s, v8::Object>>> {
    let mut out = Vec::new();
    for i in from..args.length() {
        out.push(numberish(scope, args.get(i), ctx)?);
    }
    Some(out)
}

// ── serialization (Blink `BuildCSSText`) ───────────────────────────────────────────────────────────

fn css_text(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, nested: bool, paren_less: bool, out: &mut String) {
    let Some(k) = kind(scope, o) else { return };
    let open = |out: &mut String| {
        if !paren_less {
            out.push_str(if nested { "(" } else { "calc(" });
        }
    };
    let close = |out: &mut String| {
        if !paren_less {
            out.push(')');
        }
    };
    match k {
        Kind::Unit => {
            let (v, u) = unit_parts(scope, o);
            out.push_str(&fmt_number(v));
            out.push_str(unit_suffix(&u));
        }
        Kind::Sum | Kind::Product => {
            open(out);
            let items = children(scope, o);
            for (i, c) in items.iter().enumerate() {
                let ck = kind(scope, *c);
                if i == 0 {
                    css_text(scope, *c, true, false, out);
                    continue;
                }
                let (inv, sep, plain) = if k == Kind::Sum { (Kind::Negate, " - ", " + ") } else { (Kind::Invert, " / ", " * ") };
                if ck == Some(inv) {
                    out.push_str(sep);
                    let inner = children(scope, *c);
                    if let Some(v) = inner.first() {
                        css_text(scope, *v, true, false, out);
                    }
                } else {
                    out.push_str(plain);
                    css_text(scope, *c, true, false, out);
                }
            }
            close(out);
        }
        Kind::Negate | Kind::Invert => {
            open(out);
            out.push_str(if k == Kind::Negate { "-" } else { "1 / " });
            if let Some(v) = children(scope, o).first() {
                css_text(scope, *v, true, false, out);
            }
            close(out);
        }
        Kind::Min | Kind::Max | Kind::Clamp => {
            out.push_str(match k {
                Kind::Min => "min(",
                Kind::Max => "max(",
                _ => "clamp(",
            });
            for (i, c) in children(scope, o).iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                css_text(scope, *c, true, true, out);
            }
            out.push(')');
        }
    }
}
fn numeric_text(scope: &mut v8::PinScope, o: v8::Local<v8::Object>) -> String {
    let mut s = String::new();
    css_text(scope, o, false, false, &mut s);
    s
}

// ── sum values (Blink `CSSNumericSumValue`) ────────────────────────────────────────────────────────

type UnitMap = std::collections::BTreeMap<String, i32>;
type Term = (f64, UnitMap);

fn sum_value(scope: &mut v8::PinScope, o: v8::Local<v8::Object>) -> Option<Vec<Term>> {
    let k = kind(scope, o)?;
    let kids = children(scope, o);
    match k {
        Kind::Unit => {
            let (v, u) = unit_parts(scope, o);
            let (_, _, factor, canon) = unit_info(&u);
            let mut m = UnitMap::new();
            if u != "number" {
                m.insert(canon.unwrap_or(&u).to_string(), 1);
            }
            Some(vec![(v * factor, m)])
        }
        Kind::Sum => {
            let mut out: Vec<Term> = Vec::new();
            for c in kids {
                for (v, m) in sum_value(scope, c)? {
                    match out.iter_mut().find(|t| t.1 == m) {
                        Some(t) => t.0 += v,
                        None => out.push((v, m)),
                    }
                }
            }
            Some(out)
        }
        Kind::Negate => {
            let mut s = sum_value(scope, *kids.first()?)?;
            for t in s.iter_mut() {
                t.0 *= -1.0;
            }
            Some(s)
        }
        Kind::Invert => {
            let mut s = sum_value(scope, *kids.first()?)?;
            if s.len() != 1 {
                return None;
            }
            for v in s[0].1.values_mut() {
                *v = -*v;
            }
            s[0].0 = 1.0 / s[0].0;
            Some(s)
        }
        Kind::Product => {
            let mut sum: Vec<Term> = vec![(1.0, UnitMap::new())];
            for c in kids {
                let child = sum_value(scope, c)?;
                let mut next = Vec::new();
                for a in &sum {
                    for b in &child {
                        let mut m = a.1.clone();
                        for (u, e) in &b.1 {
                            let n = m.get(u).copied().unwrap_or(0) + e;
                            if n == 0 {
                                m.remove(u);
                            } else {
                                m.insert(u.clone(), n);
                            }
                        }
                        next.push((a.0 * b.0, m));
                    }
                }
                sum = next;
            }
            Some(sum)
        }
        Kind::Min | Kind::Max | Kind::Clamp => {
            let mut sums = Vec::new();
            for c in kids {
                let s = sum_value(scope, c)?;
                if s.len() != 1 {
                    return None;
                }
                sums.push(s.into_iter().next()?);
            }
            let first_units = sums.first()?.1.clone();
            if sums.iter().any(|t| t.1 != first_units) {
                return None;
            }
            let v = match k {
                Kind::Min => sums.iter().map(|t| t.0).fold(f64::INFINITY, f64::min),
                Kind::Max => sums.iter().map(|t| t.0).fold(f64::NEG_INFINITY, f64::max),
                _ => sums[0].0.max(sums[1].0.min(sums[2].0)),
            };
            Some(vec![(v, first_units)])
        }
    }
}
/// A sum-value term as a unit value: no units is a number, one unit to the first power is that unit.
fn term_unit(t: &Term) -> Option<(f64, String)> {
    if t.1.is_empty() {
        return Some((t.0, "number".into()));
    }
    if t.1.len() == 1 {
        let (u, e) = t.1.iter().next()?;
        if *e == 1 {
            return Some((t.0, u.clone()));
        }
    }
    None
}
/// `CSSUnitValue::ConvertTo`.
fn convert(v: f64, from: &str, to: &str) -> Option<f64> {
    if from == to {
        return Some(v);
    }
    let (_, _, ff, fc) = unit_info(from);
    let (_, _, tf, tc) = unit_info(to);
    match (fc, tc) {
        (Some(a), Some(b)) if a == b => Some(v * ff / tf),
        _ => None,
    }
}

fn equals(scope: &mut v8::PinScope, a: v8::Local<v8::Object>, b: v8::Local<v8::Object>) -> bool {
    let (Some(ka), Some(kb)) = (kind(scope, a), kind(scope, b)) else { return false };
    if ka != kb {
        return false;
    }
    if ka == Kind::Unit {
        return unit_parts(scope, a) == unit_parts(scope, b);
    }
    let ca = children(scope, a);
    let cb = children(scope, b);
    ca.len() == cb.len() && ca.iter().zip(cb.iter()).all(|(x, y)| equals(scope, *x, *y))
}

// ── arithmetic (Blink `CSSNumericValue::add` ...) ──────────────────────────────────────────────────

fn negate<'s>(scope: &mut v8::PinScope<'s, '_>, o: v8::Local<'s, v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    match kind(scope, o)? {
        Kind::Unit => {
            let (v, u) = unit_parts(scope, o);
            new_unit(scope, -v, &u)
        }
        Kind::Negate => children(scope, o).into_iter().next(),
        _ => new_math(scope, Kind::Negate, &[o]),
    }
}
/// `None` with nothing pending = divide by zero.
fn invert<'s>(scope: &mut v8::PinScope<'s, '_>, o: v8::Local<'s, v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    match kind(scope, o)? {
        Kind::Unit => {
            let (v, u) = unit_parts(scope, o);
            if u == "number" {
                if v == 0.0 {
                    return None;
                }
                return new_unit(scope, 1.0 / v, "number");
            }
            new_math(scope, Kind::Invert, &[o])
        }
        Kind::Invert => children(scope, o).into_iter().next(),
        _ => new_math(scope, Kind::Invert, &[o]),
    }
}

fn arith<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments<'s>, op: &str) -> Option<v8::Local<'s, v8::Object>> {
    let this = args.this();
    let ctx = format!("Failed to execute '{op}' on 'CSSNumericValue': ");
    let mut values = numberishes(scope, args, 0, &ctx)?;
    match op {
        "sub" => {
            let mut out = Vec::new();
            for v in values {
                out.push(negate(scope, v)?);
            }
            values = out;
        }
        "div" => {
            let mut out = Vec::new();
            for v in values {
                match invert(scope, v) {
                    Some(i) => out.push(i),
                    None => {
                        throw_range_error(scope, &format!("{ctx}Can't divide-by-zero"));
                        return None;
                    }
                }
            }
            values = out;
        }
        _ => {}
    }
    let target = match op {
        "add" | "sub" => Kind::Sum,
        "mul" | "div" => Kind::Product,
        "min" => Kind::Min,
        _ => Kind::Max,
    };
    // PrependValueForArithmetic: `this`'s own operands when it is the same kind of node.
    let mut all = if kind(scope, this) == Some(target) { children(scope, this) } else { vec![this] };
    all.extend(values);
    if target == Kind::Product {
        // MaybeMultiplyAsUnitValue: unit values, at most one of them not a number.
        let mut unit = "number".to_string();
        let mut prod = 1.0;
        let mut ok = true;
        for o in &all {
            if kind(scope, *o) != Some(Kind::Unit) {
                ok = false;
                break;
            }
            let (v, u) = unit_parts(scope, *o);
            if u != "number" {
                if unit != "number" {
                    ok = false;
                    break;
                }
                unit = u;
            }
            prod *= v;
        }
        if ok {
            return new_unit(scope, prod, &unit);
        }
    } else {
        // MaybeSimplifyAsUnitValue: unit values of one unit.
        let first = all[0];
        if kind(scope, first) == Some(Kind::Unit) {
            let (mut acc, u0) = unit_parts(scope, first);
            let mut ok = true;
            for o in &all[1..] {
                if kind(scope, *o) != Some(Kind::Unit) {
                    ok = false;
                    break;
                }
                let (v, u) = unit_parts(scope, *o);
                if u != u0 {
                    ok = false;
                    break;
                }
                acc = match target {
                    Kind::Sum => acc + v,
                    Kind::Min => acc.min(v),
                    _ => acc.max(v),
                };
            }
            if ok {
                return new_unit(scope, acc, &u0);
            }
        }
    }
    create_math(scope, target, &all, &ctx)
}

// ── CSSNumericValue.parse (Blink's math-expression parser, the parts Typed OM sees) ───────────────

#[derive(Clone, Debug)]
enum Calc {
    Lit(f64, String),
    /// `+ - * /`, left, right, and whether it stands in its own parentheses (`IsNestedCalc`).
    Op(char, Box<Calc>, Box<Calc>, bool),
    Min(Vec<Calc>),
    Max(Vec<Calc>),
    Clamp(Box<Calc>, Box<Calc>, Box<Calc>),
}

fn calc_type(c: &Calc) -> Option<NType> {
    match c {
        Calc::Lit(_, u) => Some(NType::of_unit(u)),
        Calc::Op(op, l, r, _) => {
            let (a, b) = (calc_type(l)?, calc_type(r)?);
            match op {
                '+' | '-' => NType::add(a, b),
                '*' => NType::mul(a, b),
                _ => NType::mul(a, b.negate()),
            }
        }
        Calc::Min(v) | Calc::Max(v) => {
            let mut t = calc_type(v.first()?)?;
            for x in &v[1..] {
                t = NType::add(t, calc_type(x)?)?;
            }
            Some(t)
        }
        Calc::Clamp(a, b, c) => NType::add(NType::add(calc_type(a)?, calc_type(b)?)?, calc_type(c)?),
    }
}

struct CalcParser<'i, 't, 'a> {
    p: &'a mut cssparser::Parser<'i, 't>,
}
impl CalcParser<'_, '_, '_> {
    fn sum(&mut self) -> Option<Calc> {
        let mut left = self.product()?;
        loop {
            let state = self.p.state();
            match self.p.next_including_whitespace() {
                Ok(cssparser::Token::WhiteSpace(_)) => {}
                _ => {
                    self.p.reset(&state);
                    return Some(left);
                }
            }
            let state2 = self.p.state();
            let op = match self.p.next() {
                Ok(cssparser::Token::Delim('+')) => '+',
                Ok(cssparser::Token::Delim('-')) => '-',
                _ => {
                    self.p.reset(&state2);
                    return Some(left);
                }
            };
            // `+` / `-` need whitespace on both sides.
            match self.p.next_including_whitespace() {
                Ok(cssparser::Token::WhiteSpace(_)) => {}
                _ => return None,
            }
            let right = self.product()?;
            left = Calc::Op(op, Box::new(left), Box::new(right), false);
        }
    }
    fn product(&mut self) -> Option<Calc> {
        let mut left = self.value()?;
        loop {
            let state = self.p.state();
            let op = match self.p.next() {
                Ok(cssparser::Token::Delim('*')) => '*',
                Ok(cssparser::Token::Delim('/')) => '/',
                _ => {
                    self.p.reset(&state);
                    return Some(left);
                }
            };
            let right = self.value()?;
            left = Calc::Op(op, Box::new(left), Box::new(right), false);
        }
    }
    fn value(&mut self) -> Option<Calc> {
        let tok = self.p.next().ok()?.clone();
        match tok {
            cssparser::Token::Number { value, .. } => Some(Calc::Lit(value as f64, "number".into())),
            cssparser::Token::Percentage { unit_value, .. } => Some(Calc::Lit(unit_value as f64 * 100.0, "percent".into())),
            cssparser::Token::Dimension { value, unit, .. } => {
                let u = unit_from_name(&unit)?;
                if u == "fr" || u == "percent" || u == "number" {
                    return None;
                }
                Some(Calc::Lit(value as f64, u.into()))
            }
            cssparser::Token::ParenthesisBlock => {
                let r = self.p.parse_nested_block(|p| -> Result<Calc, cssparser::ParseError<'_, ()>> {
                    let mut cp = CalcParser { p };
                    let v = cp.sum().ok_or_else(|| cp.p.new_custom_error(()))?;
                    cp.p.expect_exhausted().map_err(|_| cp.p.new_custom_error(()))?;
                    Ok(v)
                });
                r.ok().map(mark_nested)
            }
            cssparser::Token::Function(name) => {
                let name = name.to_ascii_lowercase();
                self.p.parse_nested_block(|p| -> Result<Calc, cssparser::ParseError<'_, ()>> { function_body(p, &name).ok_or_else(|| p.new_custom_error(())) }).ok()
            }
            _ => None,
        }
    }
}
fn mark_nested(c: Calc) -> Calc {
    match c {
        Calc::Op(o, l, r, _) => Calc::Op(o, l, r, true),
        x => x,
    }
}
fn function_body(p: &mut cssparser::Parser, name: &str) -> Option<Calc> {
    let mut args = Vec::new();
    loop {
        let v = {
            let mut cp = CalcParser { p };
            cp.sum()?
        };
        args.push(v);
        if p.is_exhausted() {
            break;
        }
        p.expect_comma().ok()?;
    }
    match name {
        "calc" | "-webkit-calc" if args.len() == 1 => args.pop().map(mark_nested),
        "min" => Some(Calc::Min(args)),
        "max" => Some(Calc::Max(args)),
        "clamp" if args.len() == 3 => {
            let c = args.pop()?;
            let b = args.pop()?;
            let a = args.pop()?;
            Some(Calc::Clamp(Box::new(a), Box::new(b), Box::new(c)))
        }
        _ => None,
    }
}

/// Blink's simplification of a parsed expression, as far as Typed OM can see it: numbers fold,
/// like terms of a sum combine at their first position (canonical unit when the units differ but
/// convert), a number multiplies into a lone dimension or distributes over a sum of literals.
fn simplify(c: Calc) -> Option<Calc> {
    match c {
        Calc::Lit(..) => Some(c),
        Calc::Min(v) | Calc::Max(v) if v.len() == 1 => simplify(v.into_iter().next()?),
        Calc::Min(v) => Some(Calc::Min(v.into_iter().map(simplify).collect::<Option<_>>()?)),
        Calc::Max(v) => Some(Calc::Max(v.into_iter().map(simplify).collect::<Option<_>>()?)),
        Calc::Clamp(a, b, c) => Some(Calc::Clamp(Box::new(simplify(*a)?), Box::new(simplify(*b)?), Box::new(simplify(*c)?))),
        Calc::Op(op @ ('+' | '-'), l, r, nested) => {
            let mut terms: Vec<(char, Calc)> = Vec::new();
            flatten_sum(simplify(*l)?, '+', &mut terms);
            flatten_sum(simplify(*r)?, op, &mut terms);
            let mut out = combine_terms(terms)?;
            if let Calc::Op(o, a, b, _) = out {
                out = Calc::Op(o, a, b, nested);
            }
            Some(out)
        }
        Calc::Op(op, l, r, nested) => {
            let (l, r) = (simplify(*l)?, simplify(*r)?);
            if op == '/' && !matches!(&r, Calc::Lit(_, u) if u == "number") && calc_type(&r).is_none_or(|t| !t.is_number()) {
                return None;
            }
            // A number distributes over a sum of literals (`MaybeDistributeArithmeticOperation`).
            if let (Calc::Op(so @ ('+' | '-'), a, b, _), Calc::Lit(n, u)) = (&l, &r) {
                if u == "number" && matches!(**a, Calc::Lit(..)) && matches!(**b, Calc::Lit(..)) {
                    let na = simplify(Calc::Op(op, a.clone(), Box::new(Calc::Lit(*n, "number".into())), false))?;
                    let nb = simplify(Calc::Op(op, b.clone(), Box::new(Calc::Lit(*n, "number".into())), false))?;
                    return Some(mark_nested(combine_terms(vec![('+', na), (*so, nb)])?));
                }
            }
            if let (Calc::Lit(n, u), Calc::Op(so @ ('+' | '-'), a, b, _)) = (&l, &r) {
                if u == "number" && op == '*' && matches!(**a, Calc::Lit(..)) && matches!(**b, Calc::Lit(..)) {
                    let na = simplify(Calc::Op('*', Box::new(Calc::Lit(*n, "number".into())), a.clone(), false))?;
                    let nb = simplify(Calc::Op('*', Box::new(Calc::Lit(*n, "number".into())), b.clone(), false))?;
                    return Some(mark_nested(combine_terms(vec![('+', na), (*so, nb)])?));
                }
            }
            match (&l, &r) {
                (Calc::Lit(a, ua), Calc::Lit(b, ub)) if ua == "number" || ub == "number" => {
                    if op == '/' && ub != "number" {
                        return None;
                    }
                    let unit = if ua == "number" { ub.clone() } else { ua.clone() };
                    let v = if op == '*' { a * b } else { a / b };
                    Some(Calc::Lit(v, unit))
                }
                _ => {
                    let t = Calc::Op(op, Box::new(l), Box::new(r), nested);
                    // Two dimensions multiplied are not a CSS type (Typed Arithmetic is off in 150).
                    let ty = calc_type(&t)?;
                    if ty.e.iter().any(|e| e.abs() > 1) || ty.e.iter().filter(|e| **e != 0).count() > 1 {
                        return None;
                    }
                    Some(t)
                }
            }
        }
    }
}
fn flatten_sum(c: Calc, sign: char, out: &mut Vec<(char, Calc)>) {
    match c {
        Calc::Op(op @ ('+' | '-'), l, r, _) => {
            flatten_sum(*l, sign, out);
            let s = if sign == '-' { if op == '-' { '+' } else { '-' } } else { op };
            flatten_sum(*r, s, out);
        }
        x => out.push((sign, x)),
    }
}
fn combine_terms(terms: Vec<(char, Calc)>) -> Option<Calc> {
    // Combine literals of one unit, or of one category with a canonical unit.
    let group = |u: &str| -> String {
        let (_, _, _, canon) = unit_info(u);
        match canon {
            Some(c) => format!("={c}"),
            None => u.to_string(),
        }
    };
    let mut slots: Vec<(char, Calc)> = Vec::new();
    let mut seen: Vec<(String, usize)> = Vec::new();
    for (sign, t) in terms {
        if let Calc::Lit(v, u) = &t {
            let g = group(u);
            let sv = if sign == '-' { -*v } else { *v };
            if let Some(entry) = seen.iter_mut().find(|e| e.0 == g) {
                let idx = entry.1;
                if let (_, Calc::Lit(acc, au)) = &mut slots[idx] {
                    if *au == *u {
                        *acc += sv;
                    } else {
                        let (_, _, fa, canon) = unit_info(au);
                        let (_, _, fu, _) = unit_info(u);
                        *acc = *acc * fa + sv * fu;
                        *au = canon.unwrap_or("number").to_string();
                    }
                }
                slots[idx].0 = '+';
                continue;
            }
            seen.push((g, slots.len()));
            slots.push((sign, Calc::Lit(*v, u.clone())));
            continue;
        }
        slots.push((sign, t));
    }
    let mut it = slots.into_iter();
    let (s0, first) = it.next()?;
    let mut acc = match (s0, first) {
        ('-', Calc::Lit(v, u)) => Calc::Lit(-v, u),
        ('-', x) => Calc::Op('*', Box::new(Calc::Lit(-1.0, "number".into())), Box::new(x), false),
        (_, x) => x,
    };
    for (s, t) in it {
        acc = Calc::Op(s, Box::new(acc), Box::new(t), false);
    }
    calc_type(&acc)?;
    Some(acc)
}

/// Blink's `CalcToNumericValue`.
fn calc_to_numeric<'s>(scope: &mut v8::PinScope<'s, '_>, c: &Calc, top_calc: bool) -> Option<v8::Local<'s, v8::Object>> {
    match c {
        Calc::Lit(v, u) => {
            let unit = new_unit(scope, *v, u)?;
            if top_calc {
                return new_math(scope, Kind::Sum, &[unit]);
            }
            Some(unit)
        }
        Calc::Min(v) | Calc::Max(v) => {
            let mut items = Vec::new();
            for x in v {
                items.push(calc_to_numeric(scope, x, false)?);
            }
            new_math(scope, if matches!(c, Calc::Min(_)) { Kind::Min } else { Kind::Max }, &items)
        }
        Calc::Clamp(a, b, d) => {
            let items = [calc_to_numeric(scope, a, false)?, calc_to_numeric(scope, b, false)?, calc_to_numeric(scope, d, false)?];
            new_math(scope, Kind::Clamp, &items)
        }
        Calc::Op(op, ..) => {
            let canon = |o: char| if o == '+' || o == '-' { '+' } else { '*' };
            let root = canon(*op);
            let mut values = Vec::new();
            let mut cur = c;
            loop {
                let Calc::Op(o, l, r, _) = cur else { break };
                let v = calc_to_numeric(scope, r, false)?;
                let v = match o {
                    '-' => new_math(scope, Kind::Negate, &[v])?,
                    '/' => new_math(scope, Kind::Invert, &[v])?,
                    _ => v,
                };
                values.push(v);
                cur = l;
                match cur {
                    Calc::Op(lo, _, _, nested) if !nested && canon(*lo) == root => continue,
                    _ => break,
                }
            }
            values.push(calc_to_numeric(scope, cur, false)?);
            values.reverse();
            new_math(scope, if root == '+' { Kind::Sum } else { Kind::Product }, &values)
        }
    }
}

/// `CSSNumericValue.parse(cssText)`: `None` = SyntaxError "Invalid math expression".
pub(crate) fn parse_numeric<'s>(scope: &mut v8::PinScope<'s, '_>, text: &str) -> Option<v8::Local<'s, v8::Object>> {
    let mut input = cssparser::ParserInput::new(text);
    let mut p = cssparser::Parser::new(&mut input);
    let tok = p.next().ok()?.clone();
    match tok {
        cssparser::Token::Number { value, .. } => {
            p.expect_exhausted().ok()?;
            new_unit(scope, value as f64, "number")
        }
        cssparser::Token::Percentage { unit_value, .. } => {
            p.expect_exhausted().ok()?;
            new_unit(scope, unit_value as f64 * 100.0, "percent")
        }
        cssparser::Token::Dimension { value, unit, .. } => {
            let u = unit_from_name(&unit)?;
            p.expect_exhausted().ok()?;
            new_unit(scope, value as f64, u)
        }
        cssparser::Token::Function(name) => {
            let name = name.to_ascii_lowercase();
            if !matches!(name.as_str(), "calc" | "-webkit-calc" | "min" | "max" | "clamp") {
                return None;
            }
            let parsed = p.parse_nested_block(|p| -> Result<Calc, cssparser::ParseError<'_, ()>> { function_body(p, &name).ok_or_else(|| p.new_custom_error(())) }).ok()?;
            p.expect_exhausted().ok()?;
            let top_calc = matches!(name.as_str(), "calc" | "-webkit-calc");
            let simplified = simplify(parsed)?;
            calc_type(&simplified)?;
            let wrap = top_calc && matches!(simplified, Calc::Lit(..));
            calc_to_numeric(scope, &simplified, wrap)
        }
        _ => None,
    }
}

// ── type() ────────────────────────────────────────────────────────────────────────────────────────

fn type_dict<'s>(scope: &mut v8::PinScope<'s, '_>, t: NType) -> v8::Local<'s, v8::Object> {
    let o = v8::Object::new(scope);
    // A WebIDL dictionary converts its members in lexicographic order.
    let order = [ANGLE, FLEX, FREQUENCY, LENGTH, PERCENT, usize::MAX, RESOLUTION, TIME];
    for i in order {
        if i == usize::MAX {
            if let Some(h) = t.hint {
                let k = str_val(scope, "percentHint");
                let v = str_val(scope, BASE_NAMES[h]);
                o.set(scope, k, v);
            }
            continue;
        }
        if t.e[i] != 0 {
            let k = str_val(scope, BASE_NAMES[i]);
            let v = v8::Number::new(scope, t.e[i] as f64).into();
            o.set(scope, k, v);
        }
    }
    o
}

// ── transforms ────────────────────────────────────────────────────────────────────────────────────

fn tkind(scope: &mut v8::PinScope, o: v8::Local<v8::Object>) -> Option<String> {
    pstr(scope, o, "tkind")
}
fn tget<'s>(scope: &mut v8::PinScope<'s, '_>, o: v8::Local<v8::Object>, k: &str) -> Option<v8::Local<'s, v8::Object>> {
    pobj(scope, o, k)
}
fn is2d(scope: &mut v8::PinScope, o: v8::Local<v8::Object>) -> bool {
    match tkind(scope, o).as_deref() {
        Some("skew") | Some("skewX") | Some("skewY") => true,
        Some("perspective") => false,
        _ => pget(scope, o, "is2D").is_some_and(|v| v.is_true()),
    }
}

fn type_is(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, want: &str) -> bool {
    let Some(t) = ntype(scope, o) else { return false };
    match want {
        "lp" => t.is_length_or_percent(),
        "length" => t.is_only(LENGTH),
        "angle" => t.is_only(ANGLE),
        _ => t.is_number(),
    }
}

fn comp_text(scope: &mut v8::PinScope, o: v8::Local<v8::Object>) -> String {
    let tk = tkind(scope, o).unwrap_or_default();
    let part = |scope: &mut v8::PinScope, k: &str| -> String { tget(scope, o, k).map(|v| numeric_text(scope, v)).unwrap_or_default() };
    match tk.as_str() {
        "translate" => {
            let (x, y) = (part(scope, "x"), part(scope, "y"));
            if is2d(scope, o) {
                format!("translate({x}, {y})")
            } else {
                let z = part(scope, "z");
                format!("translate3d({x}, {y}, {z})")
            }
        }
        "rotate" => {
            let a = part(scope, "angle");
            if is2d(scope, o) {
                format!("rotate({a})")
            } else {
                let (x, y, z) = (part(scope, "x"), part(scope, "y"), part(scope, "z"));
                format!("rotate3d({x}, {y}, {z}, {a})")
            }
        }
        "scale" => {
            let (x, y) = (part(scope, "x"), part(scope, "y"));
            if is2d(scope, o) {
                format!("scale({x}, {y})")
            } else {
                let z = part(scope, "z");
                format!("scale3d({x}, {y}, {z})")
            }
        }
        "skew" => format!("skew({}, {})", part(scope, "ax"), part(scope, "ay")),
        "skewX" => format!("skewX({})", part(scope, "ax")),
        "skewY" => format!("skewY({})", part(scope, "ay")),
        "perspective" => {
            let l = pget(scope, o, "length");
            let t = match l.and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
                Some(v) if kind(scope, v).is_some() => numeric_text(scope, v),
                Some(v) => pstr(scope, v, "kw").unwrap_or_default(),
                None => String::new(),
            };
            format!("perspective({t})")
        }
        "matrix" => {
            let Some(m) = pobj(scope, o, "matrix") else { return String::new() };
            let names: &[&str] = if is2d(scope, o) {
                &["a", "b", "c", "d", "e", "f"]
            } else {
                &["m11", "m12", "m13", "m14", "m21", "m22", "m23", "m24", "m31", "m32", "m33", "m34", "m41", "m42", "m43", "m44"]
            };
            let vals: Vec<String> = names.iter().map(|n| fmt_number(get_prop(scope, m, n).and_then(|v| v.number_value(scope)).unwrap_or(0.0))).collect();
            format!("{}({})", if names.len() == 6 { "matrix" } else { "matrix3d" }, vals.join(", "))
        }
        _ => String::new(),
    }
}

fn to_unit(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, unit: &str) -> Option<f64> {
    let s = sum_value(scope, o)?;
    if s.len() != 1 {
        return None;
    }
    let (v, u) = term_unit(&s[0])?;
    convert(v, &u, unit)
}

fn new_dommatrix<'s>(scope: &mut v8::PinScope<'s, '_>, init: Option<&[f64]>) -> Option<v8::Local<'s, v8::Object>> {
    let ctor = crate::binding::protos::iface_object(scope, "DOMMatrix").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())?;
    let args: Vec<v8::Local<v8::Value>> = match init {
        Some(vals) => {
            let items: Vec<v8::Local<v8::Value>> = vals.iter().map(|n| v8::Number::new(scope, *n).into()).collect();
            vec![v8::Array::new_with_elements(scope, &items).into()]
        }
        None => vec![],
    };
    ctor.new_instance(scope, &args)
}
fn num_args<'s>(scope: &mut v8::PinScope<'s, '_>, vals: &[f64]) -> Vec<v8::Local<'s, v8::Value>> {
    vals.iter().map(|n| v8::Number::new(scope, *n).into()).collect()
}

/// A component's `toMatrix()`; `None` = an exception is pending.
fn comp_matrix<'s>(scope: &mut v8::PinScope<'s, '_>, o: v8::Local<v8::Object>, ctx: &str) -> Option<v8::Local<'s, v8::Object>> {
    let tk = tkind(scope, o).unwrap_or_default();
    let two = is2d(scope, o);
    let px_err = |scope: &mut v8::PinScope| throw_type_error(scope, &format!("{ctx}Cannot create matrix if units are not compatible with px"));
    match tk.as_str() {
        "translate" => {
            let mut vals = Vec::new();
            for k in ["x", "y", "z"] {
                let Some(v) = tget(scope, o, k) else { return None };
                match to_unit(scope, v, "px") {
                    Some(n) => vals.push(n),
                    None => {
                        px_err(scope);
                        return None;
                    }
                }
            }
            if two {
                vals.truncate(2);
            }
            let m = new_dommatrix(scope, None)?;
            let a = num_args(scope, &vals);
            call_method(scope, m.into(), "translateSelf", &a)?;
            Some(m)
        }
        "scale" => {
            let mut vals = Vec::new();
            for k in ["x", "y", "z"] {
                let v = tget(scope, o, k)?;
                vals.push(to_unit(scope, v, "number").unwrap_or(1.0));
            }
            if two {
                vals.truncate(2);
            }
            let m = new_dommatrix(scope, None)?;
            let a = num_args(scope, &vals);
            call_method(scope, m.into(), "scaleSelf", &a)?;
            Some(m)
        }
        "rotate" => {
            let mut axis = Vec::new();
            for k in ["x", "y", "z"] {
                let v = tget(scope, o, k)?;
                match to_unit(scope, v, "number") {
                    Some(n) => axis.push(n),
                    None => {
                        throw_type_error(scope, &format!("{ctx}Cannot create matrix if units cannot be converted to CSSUnitValue"));
                        return None;
                    }
                }
            }
            let ang = tget(scope, o, "angle")?;
            let deg = to_unit(scope, ang, "deg").unwrap_or(0.0);
            let args = if two { [0.0, 0.0, 1.0, deg] } else { [axis[0], axis[1], axis[2], deg] };
            let m = new_dommatrix(scope, None)?;
            let a = num_args(scope, &args);
            call_method(scope, m.into(), "rotateAxisAngleSelf", &a)?;
            Some(m)
        }
        "skew" | "skewX" | "skewY" => {
            let rad = |scope: &mut v8::PinScope, k: &str| -> f64 { tget(scope, o, k).and_then(|v| to_unit(scope, v, "deg")).unwrap_or(0.0).to_radians() };
            let ax = if tk == "skewY" { 0.0 } else { rad(scope, "ax").tan() };
            let ay = if tk == "skewX" { 0.0 } else { rad(scope, "ay").tan() };
            new_dommatrix(scope, Some(&[1.0, ay, ax, 1.0, 0.0, 0.0]))
        }
        "perspective" => {
            let l = pobj(scope, o, "length");
            match l {
                Some(v) if kind(scope, v).is_some() => {
                    if kind(scope, v) == Some(Kind::Unit) && unit_parts(scope, v).0 < 0.0 {
                        return new_dommatrix(scope, None);
                    }
                    let Some(px) = to_unit(scope, v, "px") else {
                        px_err(scope);
                        return None;
                    };
                    let m34 = if px == 0.0 { 0.0 } else { -1.0 / px };
                    new_dommatrix(scope, Some(&[1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, m34, 0.0, 0.0, 0.0, 1.0]))
                }
                _ => new_dommatrix(scope, None),
            }
        }
        "matrix" => {
            let m = pobj(scope, o, "matrix")?;
            let flag2d = get_prop(scope, m, "is2D").is_some_and(|v| v.is_true());
            if two && !flag2d {
                let vals: Vec<f64> = ["a", "b", "c", "d", "e", "f"].iter().map(|n| get_prop(scope, m, n).and_then(|v| v.number_value(scope)).unwrap_or(0.0)).collect();
                return new_dommatrix(scope, Some(&vals));
            }
            let dm = crate::binding::protos::iface_object(scope, "DOMMatrix").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
            let f = get_prop(scope, dm, "fromMatrix").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())?;
            f.call(scope, dm.into(), &[m.into()]).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        }
        _ => new_dommatrix(scope, None),
    }
}

/// The components of a `CSSTransformValue` / the segments of a `CSSUnparsedValue`: its own indexed
/// properties `0..` up to the first gap.
fn indexed<'s>(scope: &mut v8::PinScope<'s, '_>, o: v8::Local<v8::Object>) -> Vec<v8::Local<'s, v8::Value>> {
    let mut out = Vec::new();
    let mut i = 0u32;
    loop {
        let k: v8::Local<v8::Name> = v8::String::new(scope, &i.to_string()).unwrap().into();
        if !o.has_own_property(scope, k).unwrap_or(false) {
            break;
        }
        match o.get(scope, k.into()) {
            Some(v) => out.push(v),
            None => break,
        }
        i += 1;
    }
    out
}

// ── unparsed values ───────────────────────────────────────────────────────────────────────────────

fn unparsed_segments(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, out: &mut String, depth: u32) -> bool {
    if depth > 64 {
        return false;
    }
    let segs = indexed(scope, o);
    for (i, s) in segs.iter().enumerate() {
        if i > 0 {
            out.push_str("/**/");
        }
        if let Some(r) = v8::Local::<v8::Object>::try_from(*s).ok().filter(|r| pstr(scope, *r, "var").is_some()) {
            let name = pstr(scope, r, "var").unwrap_or_default();
            out.push_str("var(");
            out.push_str(&name);
            if let Some(fb) = pobj(scope, r, "fallback") {
                out.push(',');
                if !unparsed_segments(scope, fb, out, depth + 1) {
                    return false;
                }
            }
            out.push(')');
        } else {
            out.push_str(&s.to_rust_string_lossy(scope));
        }
    }
    true
}

/// Blink's `CSSUnparsedValue::ToStringInternal`: the segments joined by `/**/`, re-tokenized, and
/// `/**/` kept only where two tokens would otherwise merge.
fn unparsed_text(scope: &mut v8::PinScope, o: v8::Local<v8::Object>) -> String {
    let mut raw = String::new();
    if !unparsed_segments(scope, o, &mut raw, 0) {
        return String::new();
    }
    serialize_tokens(&raw)
}

/// Re-serialize CSS text token by token (comments dropped), with `/**/` where the CSS Syntax
/// serialization table needs one (`NeedsInsertedComment`).
pub(crate) fn serialize_tokens(text: &str) -> String {
    use cssparser::{ToCss, Token, TokenSerializationType};
    fn walk(p: &mut cssparser::Parser, out: &mut String, prev: &mut TokenSerializationType) {
        loop {
            let tok = match p.next_including_whitespace_and_comments() {
                Ok(t) => t.clone(),
                Err(_) => return,
            };
            if let Token::Comment(_) = tok {
                continue;
            }
            let ty = tok.serialization_type();
            if prev.needs_separator_when_before(ty) {
                out.push_str("/**/");
            }
            *prev = ty;
            match &tok {
                Token::WhiteSpace(_) => out.push(' '),
                t => {
                    let _ = t.to_css(out);
                }
            }
            let close = match tok {
                Token::Function(_) | Token::ParenthesisBlock => Some(')'),
                Token::SquareBracketBlock => Some(']'),
                Token::CurlyBracketBlock => Some('}'),
                _ => None,
            };
            if let Some(c) = close {
                let _ = p.parse_nested_block(|p| -> Result<(), cssparser::ParseError<'_, ()>> {
                    walk(p, out, prev);
                    Ok(())
                });
                out.push(c);
                *prev = TokenSerializationType::default();
            }
        }
    }
    let mut input = cssparser::ParserInput::new(text);
    let mut p = cssparser::Parser::new(&mut input);
    let mut out = String::new();
    let mut prev = TokenSerializationType::default();
    walk(&mut p, &mut out, &mut prev);
    out
}

// ── toString for every Typed OM value ─────────────────────────────────────────────────────────────

/// What `CSSStyleValue.prototype.toString` answers for a Typed OM value, or `None` for anything else.
pub(crate) fn style_value_text(scope: &mut v8::PinScope, o: v8::Local<v8::Object>) -> Option<String> {
    if kind(scope, o).is_some() {
        return Some(numeric_text(scope, o));
    }
    if let Some(k) = pstr(scope, o, "kw") {
        return Some(k);
    }
    if tkind(scope, o).is_some() {
        return Some(comp_text(scope, o));
    }
    match pstr(scope, o, "list").as_deref() {
        Some("transform") => {
            let comps = indexed(scope, o);
            let parts: Vec<String> = comps.iter().filter_map(|c| v8::Local::<v8::Object>::try_from(*c).ok()).map(|c| comp_text(scope, c)).collect();
            Some(parts.join(" "))
        }
        Some("unparsed") => Some(unparsed_text(scope, o)),
        _ => None,
    }
}

// ── constructors (`binding::protos::cb_shell_ctor`) ───────────────────────────────────────────────

const TOM_IFACES: &[&str] = &[
    "CSSUnitValue", "CSSKeywordValue", "CSSMathSum", "CSSMathProduct", "CSSMathMin", "CSSMathMax", "CSSMathNegate", "CSSMathInvert",
    "CSSMathClamp", "CSSTranslate", "CSSRotate", "CSSScale", "CSSSkew", "CSSSkewX", "CSSSkewY", "CSSPerspective", "CSSMatrixComponent",
    "CSSTransformValue", "CSSUnparsedValue", "CSSVariableReferenceValue",
];

/// The Typed OM constructors. `true` = this was one (constructed, or an exception is pending).
pub(crate) fn construct<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str, this: v8::Local<'s, v8::Object>, args: &v8::FunctionCallbackArguments<'s>) -> bool {
    if !TOM_IFACES.contains(&iface) {
        return false;
    }
    let ctx = format!("Failed to construct '{iface}': ");
    let _ = construct_inner(scope, iface, this, args, &ctx);
    true
}

fn construct_inner<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str, this: v8::Local<'s, v8::Object>, args: &v8::FunctionCallbackArguments<'s>, ctx: &str) -> Option<()> {
    match iface {
        "CSSUnitValue" => {
            let v = args.get(0).number_value(scope)?;
            if !v.is_finite() {
                throw_type_error(scope, &format!("{ctx}The provided double value is non-finite."));
                return None;
            }
            let raw = args.get(1).to_string(scope)?.to_rust_string_lossy(scope);
            let Some(u) = unit_from_name(&raw) else {
                throw_type_error(scope, &format!("{ctx}Invalid unit: {raw}"));
                return None;
            };
            init_unit(scope, this, v, u);
        }
        "CSSKeywordValue" => {
            let s = args.get(0).to_string(scope)?.to_rust_string_lossy(scope);
            if s.is_empty() {
                throw_type_error(scope, &format!("{ctx}CSSKeywordValue does not support empty strings"));
                return None;
            }
            pset_str(scope, this, "kw", &s);
        }
        "CSSMathSum" | "CSSMathProduct" | "CSSMathMin" | "CSSMathMax" => {
            let items = {
                let mut out = Vec::new();
                for i in 0..args.length() {
                    out.push(numberish(scope, args.get(i), ctx)?);
                }
                out
            };
            if items.is_empty() {
                throw_dom(scope, &format!("{ctx}Arguments can't be empty"), "SyntaxError");
                return None;
            }
            let k = match iface {
                "CSSMathSum" => Kind::Sum,
                "CSSMathProduct" => Kind::Product,
                "CSSMathMin" => Kind::Min,
                _ => Kind::Max,
            };
            let op = if k == Kind::Product { NType::mul } else { NType::add };
            if type_check(scope, &items, op).is_none() {
                throw_type_error(scope, &format!("{ctx}Incompatible types"));
                return None;
            }
            init_math(scope, this, k, &items);
        }
        "CSSMathNegate" | "CSSMathInvert" => {
            let v = numberish(scope, args.get(0), ctx)?;
            init_math(scope, this, if iface == "CSSMathNegate" { Kind::Negate } else { Kind::Invert }, &[v]);
        }
        "CSSMathClamp" => {
            let items = [numberish(scope, args.get(0), ctx)?, numberish(scope, args.get(1), ctx)?, numberish(scope, args.get(2), ctx)?];
            if type_check(scope, &items, NType::add).is_none() {
                throw_type_error(scope, &format!("{ctx}Incompatible types"));
                return None;
            }
            init_math(scope, this, Kind::Clamp, &items);
        }
        "CSSTranslate" => {
            let mut parts = Vec::new();
            for i in 0..args.length().min(3) {
                let Some((_, o)) = numeric(scope, args.get(i)) else {
                    throw_type_error(scope, &format!("{ctx}parameter {} is not of type 'CSSNumericValue'.", i + 1));
                    return None;
                };
                parts.push(o);
            }
            let has_z = parts.len() == 3;
            let ok = type_is(scope, parts[0], "lp") && type_is(scope, parts[1], "lp") && (!has_z || type_is(scope, parts[2], "length"));
            if !ok {
                let msg = if has_z { "Must pass length or percentage to X, Y and Z of CSSTranslate" } else { "Must pass length or percentage to X and Y of CSSTranslate" };
                throw_type_error(scope, &format!("{ctx}{msg}"));
                return None;
            }
            let z = if has_z { parts[2] } else { new_unit(scope, 0.0, "px")? };
            pset_str(scope, this, "tkind", "translate");
            pset(scope, this, "x", parts[0].into());
            pset(scope, this, "y", parts[1].into());
            pset(scope, this, "z", z.into());
            pset_bool(scope, this, "is2D", !has_z);
        }
        "CSSRotate" => {
            let (x, y, z, angle, two) = if args.length() >= 4 {
                let x = numberish(scope, args.get(0), ctx)?;
                let y = numberish(scope, args.get(1), ctx)?;
                let z = numberish(scope, args.get(2), ctx)?;
                let Some((_, a)) = numeric(scope, args.get(3)) else {
                    throw_type_error(scope, &format!("{ctx}parameter 4 is not of type 'CSSNumericValue'."));
                    return None;
                };
                (x, y, z, a, false)
            } else {
                let Some((_, a)) = numeric(scope, args.get(0)) else {
                    throw_type_error(scope, &format!("{ctx}parameter 1 is not of type 'CSSNumericValue'."));
                    return None;
                };
                (new_unit(scope, 0.0, "number")?, new_unit(scope, 0.0, "number")?, new_unit(scope, 1.0, "number")?, a, true)
            };
            if !type_is(scope, angle, "angle") {
                throw_type_error(scope, &format!("{ctx}Must pass an angle to CSSRotate"));
                return None;
            }
            if !(type_is(scope, x, "number") && type_is(scope, y, "number") && type_is(scope, z, "number")) {
                throw_type_error(scope, &format!("{ctx}Must specify an number unit"));
                return None;
            }
            pset_str(scope, this, "tkind", "rotate");
            for (k, v) in [("x", x), ("y", y), ("z", z), ("angle", angle)] {
                pset(scope, this, k, v.into());
            }
            pset_bool(scope, this, "is2D", two);
        }
        "CSSScale" => {
            let x = numberish(scope, args.get(0), ctx)?;
            let y = numberish(scope, args.get(1), ctx)?;
            let has_z = args.length() >= 3 && !args.get(2).is_undefined();
            let z = if has_z { numberish(scope, args.get(2), ctx)? } else { new_unit(scope, 1.0, "number")? };
            if !(type_is(scope, x, "number") && type_is(scope, y, "number") && type_is(scope, z, "number")) {
                throw_type_error(scope, &format!("{ctx}Must specify an number unit"));
                return None;
            }
            pset_str(scope, this, "tkind", "scale");
            for (k, v) in [("x", x), ("y", y), ("z", z)] {
                pset(scope, this, k, v.into());
            }
            pset_bool(scope, this, "is2D", !has_z);
        }
        "CSSSkew" | "CSSSkewX" | "CSSSkewY" => {
            let names: &[&str] = match iface {
                "CSSSkew" => &["ax", "ay"],
                "CSSSkewX" => &["ax"],
                _ => &["ay"],
            };
            let mut vals = Vec::new();
            for (i, _) in names.iter().enumerate() {
                let Some((_, o)) = numeric(scope, args.get(i as i32)) else {
                    throw_type_error(scope, &format!("{ctx}parameter {} is not of type 'CSSNumericValue'.", i + 1));
                    return None;
                };
                if !type_is(scope, o, "angle") {
                    throw_type_error(scope, &format!("{ctx}{iface} does not support non-angles"));
                    return None;
                }
                vals.push(o);
            }
            let tk = match iface {
                "CSSSkew" => "skew",
                "CSSSkewX" => "skewX",
                _ => "skewY",
            };
            pset_str(scope, this, "tkind", tk);
            for (k, v) in names.iter().zip(vals) {
                pset(scope, this, k, v.into());
            }
        }
        "CSSPerspective" => {
            let v = args.get(0);
            let length: v8::Local<v8::Value> = if let Some((_, o)) = numeric(scope, v) {
                if !type_is(scope, o, "length") {
                    throw_type_error(scope, &format!("{ctx}Must pass length or none to CSSPerspective"));
                    return None;
                }
                o.into()
            } else {
                // CSSKeywordish: a CSSKeywordValue or a string, and only `none`.
                let kw = match v8::Local::<v8::Object>::try_from(v).ok().and_then(|o| pstr(scope, o, "kw")) {
                    Some(k) => Some((k, v)),
                    None if v.is_string() => Some((v.to_rust_string_lossy(scope), v)),
                    None => None,
                };
                match kw {
                    Some((k, _)) if k.eq_ignore_ascii_case("none") => {
                        let o = mint(scope, "CSSKeywordValue")?;
                        pset_str(scope, o, "kw", &k);
                        o.into()
                    }
                    _ => {
                        throw_type_error(scope, &format!("{ctx}Must pass length or none to CSSPerspective"));
                        return None;
                    }
                }
            };
            pset_str(scope, this, "tkind", "perspective");
            pset(scope, this, "length", length);
        }
        "CSSMatrixComponent" => {
            let m = args.get(0);
            if !crate::binding::protos::implements(scope, m, "DOMMatrixReadOnly") {
                throw_type_error(scope, &format!("{ctx}parameter 1 is not of type 'DOMMatrixReadOnly'."));
                return None;
            }
            let dm = crate::binding::protos::iface_object(scope, "DOMMatrix").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
            let f = get_prop(scope, dm, "fromMatrix").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())?;
            let copy = f.call(scope, dm.into(), &[m])?;
            let mo = v8::Local::<v8::Object>::try_from(m).ok()?;
            let default_2d = get_prop(scope, mo, "is2D").is_some_and(|v| v.is_true());
            let two = match v8::Local::<v8::Object>::try_from(args.get(1)).ok().and_then(|o| get_prop(scope, o, "is2D")).filter(|v| !v.is_undefined()) {
                Some(v) => v.boolean_value(scope),
                None => default_2d,
            };
            pset_str(scope, this, "tkind", "matrix");
            pset(scope, this, "matrix", copy);
            pset_bool(scope, this, "is2D", two);
        }
        "CSSTransformValue" => {
            let list = sequence(scope, args.get(0), ctx)?;
            let mut comps = Vec::new();
            for v in list {
                let ok = v8::Local::<v8::Object>::try_from(v).ok().filter(|o| tkind(scope, *o).is_some());
                let Some(o) = ok else {
                    throw_type_error(scope, &format!("{ctx}Failed to convert value to 'CSSTransformComponent'."));
                    return None;
                };
                comps.push(o);
            }
            if comps.is_empty() {
                throw_type_error(scope, &format!("{ctx}CSSTransformValue must have at least one component"));
                return None;
            }
            pset_str(scope, this, "list", "transform");
            for (i, c) in comps.iter().enumerate() {
                define_index(scope, this, i as u32, (*c).into(), true);
            }
        }
        "CSSUnparsedValue" => {
            let list = sequence(scope, args.get(0), ctx)?;
            let mut segs = Vec::new();
            for v in list {
                let is_ref = v8::Local::<v8::Object>::try_from(v).ok().is_some_and(|o| pstr(scope, o, "var").is_some());
                if is_ref {
                    segs.push(v);
                } else {
                    segs.push(v.to_string(scope)?.into());
                }
            }
            pset_str(scope, this, "list", "unparsed");
            for (i, s) in segs.iter().enumerate() {
                define_index(scope, this, i as u32, *s, true);
            }
        }
        "CSSVariableReferenceValue" => {
            let name = args.get(0).to_string(scope)?.to_rust_string_lossy(scope);
            if !is_custom_property_name(&name) {
                throw_type_error(scope, &format!("{ctx}Invalid custom property name"));
                return None;
            }
            let fb = args.get(1);
            if !fb.is_null_or_undefined() {
                let ok = v8::Local::<v8::Object>::try_from(fb).ok().is_some_and(|o| pstr(scope, o, "list").as_deref() == Some("unparsed"));
                if !ok {
                    throw_type_error(scope, &format!("{ctx}parameter 2 is not of type 'CSSUnparsedValue'."));
                    return None;
                }
                pset(scope, this, "fallback", fb);
            }
            pset_str(scope, this, "var", &name);
        }
        _ => {}
    }
    Some(())
}

fn is_custom_property_name(s: &str) -> bool {
    s.len() >= 2 && s.starts_with("--")
}

/// A WebIDL `sequence<T>` from an iterable; `None` = an exception is pending.
fn sequence<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'s, v8::Value>, ctx: &str) -> Option<Vec<v8::Local<'s, v8::Value>>> {
    let Ok(o) = v8::Local::<v8::Object>::try_from(v) else {
        throw_type_error(scope, &format!("{ctx}The provided value cannot be converted to a sequence."));
        return None;
    };
    let sym = v8::Symbol::get_iterator(scope);
    let f = o.get(scope, sym.into()).and_then(|f| v8::Local::<v8::Function>::try_from(f).ok());
    let Some(f) = f else {
        throw_type_error(scope, &format!("{ctx}The object must have a callable @@iterator property."));
        return None;
    };
    let it = f.call(scope, o.into(), &[]).and_then(|i| v8::Local::<v8::Object>::try_from(i).ok())?;
    let next = get_prop(scope, it, "next").and_then(|n| v8::Local::<v8::Function>::try_from(n).ok())?;
    let mut out = Vec::new();
    loop {
        let r = next.call(scope, it.into(), &[]).and_then(|r| v8::Local::<v8::Object>::try_from(r).ok())?;
        if get_prop(scope, r, "done")?.boolean_value(scope) {
            break;
        }
        out.push(get_prop(scope, r, "value")?);
    }
    Some(out)
}

// ── member bodies ─────────────────────────────────────────────────────────────────────────────────

fn split_data(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> (String, String) {
    let d = body_data(scope, args).to_rust_string_lossy(scope);
    let (a, b) = d.split_once('.').unwrap_or(("", d.as_str()));
    (a.to_string(), b.to_string())
}

fn cb_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let (iface, name) = split_data(scope, &args);
    let this = args.this();
    let ok = match iface.as_str() {
        "CSSUnitValue" => kind(scope, this) == Some(Kind::Unit),
        "CSSMathValue" => kind(scope, this).is_some_and(|k| k != Kind::Unit),
        i if i.starts_with("CSSMath") => kind(scope, this).is_some_and(|k| k.iface() == i),
        "CSSKeywordValue" => pstr(scope, this, "kw").is_some(),
        "CSSNumericArray" => pget(scope, this, "items").is_some(),
        "CSSTransformValue" => pstr(scope, this, "list").as_deref() == Some("transform"),
        "CSSUnparsedValue" => pstr(scope, this, "list").as_deref() == Some("unparsed"),
        "CSSVariableReferenceValue" => pstr(scope, this, "var").is_some(),
        _ => tkind(scope, this).is_some(),
    };
    if !ok {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    match (iface.as_str(), name.as_str()) {
        ("CSSUnitValue", "value") => rv.set_double(unit_parts(scope, this).0),
        ("CSSUnitValue", "unit") => {
            let u = unit_parts(scope, this).1;
            rv.set(str_val(scope, &u));
        }
        ("CSSKeywordValue", "value") => {
            let k = pstr(scope, this, "kw").unwrap_or_default();
            rv.set(str_val(scope, &k));
        }
        ("CSSMathValue", "operator") => {
            let k = kind(scope, this).map(|k| k.name()).unwrap_or("");
            rv.set(str_val(scope, k));
        }
        (_, "values") if iface.starts_with("CSSMath") => {
            if let Some(a) = pget(scope, this, "vals") {
                rv.set(a);
            }
        }
        ("CSSMathNegate" | "CSSMathInvert" | "CSSMathClamp", "value") => {
            if let Some(v) = pget(scope, this, "val") {
                rv.set(v);
            }
        }
        ("CSSMathClamp", k @ ("lower" | "upper")) => {
            if let Some(v) = pget(scope, this, k) {
                rv.set(v);
            }
        }
        ("CSSNumericArray", "length") => {
            let n = pget(scope, this, "items").and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()).map(|a| a.length()).unwrap_or(0);
            rv.set_uint32(n);
        }
        ("CSSTransformValue" | "CSSUnparsedValue", "length") => {
            let n = indexed(scope, this).len();
            rv.set_uint32(n as u32);
        }
        ("CSSTransformValue", "is2D") => {
            let comps = indexed(scope, this);
            let all = comps.iter().filter_map(|c| v8::Local::<v8::Object>::try_from(*c).ok()).all(|c| is2d(scope, c));
            rv.set_bool(all);
        }
        ("CSSVariableReferenceValue", "variable") => {
            let v = pstr(scope, this, "var").unwrap_or_default();
            rv.set(str_val(scope, &v));
        }
        ("CSSVariableReferenceValue", "fallback") => match pget(scope, this, "fallback") {
            Some(v) => rv.set(v),
            None => rv.set_null(),
        },
        ("CSSTransformComponent", "is2D") => rv.set_bool(is2d(scope, this)),
        ("CSSPerspective", "length") => {
            if let Some(v) = pget(scope, this, "length") {
                rv.set(v);
            }
        }
        ("CSSMatrixComponent", "matrix") => {
            if let Some(v) = pget(scope, this, "matrix") {
                rv.set(v);
            }
        }
        (_, k) => {
            if let Some(v) = pget(scope, this, k) {
                rv.set(v);
            }
        }
    }
}

fn cb_set<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let (iface, name) = split_data(scope, &args);
    let this = args.this();
    let ctx = format!("Failed to set the '{name}' property on '{iface}': ");
    let v = args.get(0);
    match (iface.as_str(), name.as_str()) {
        ("CSSUnitValue", "value") => {
            if kind(scope, this) != Some(Kind::Unit) {
                throw_type_error(scope, "Illegal invocation");
                return;
            }
            let Some(n) = v.number_value(scope) else { return };
            if !n.is_finite() {
                throw_type_error(scope, &format!("{ctx}The provided double value is non-finite."));
                return;
            }
            pset_num(scope, this, "v", n);
        }
        ("CSSKeywordValue", "value") => {
            if pstr(scope, this, "kw").is_none() {
                throw_type_error(scope, "Illegal invocation");
                return;
            }
            let Some(s) = v.to_string(scope) else { return };
            let s = s.to_rust_string_lossy(scope);
            if s.is_empty() {
                throw_type_error(scope, &format!("{ctx}CSSKeywordValue does not support empty strings"));
                return;
            }
            pset_str(scope, this, "kw", &s);
        }
        ("CSSVariableReferenceValue", "variable") => {
            if pstr(scope, this, "var").is_none() {
                throw_type_error(scope, "Illegal invocation");
                return;
            }
            let Some(s) = v.to_string(scope) else { return };
            let s = s.to_rust_string_lossy(scope);
            if !is_custom_property_name(&s) {
                throw_type_error(scope, &format!("{ctx}Invalid custom property name"));
                return;
            }
            pset_str(scope, this, "var", &s);
        }
        ("CSSTransformComponent", "is2D") => {
            if tkind(scope, this).is_none() {
                throw_type_error(scope, "Illegal invocation");
                return;
            }
            let b = v.boolean_value(scope);
            // Skews are always 2D and a perspective never is: their setters change nothing.
            if !matches!(tkind(scope, this).as_deref(), Some("skew" | "skewX" | "skewY" | "perspective")) {
                pset_bool(scope, this, "is2D", b);
            }
        }
        (comp, k) => {
            if tkind(scope, this).is_none() {
                throw_type_error(scope, "Illegal invocation");
                return;
            }
            if comp == "CSSMatrixComponent" {
                if !crate::binding::protos::implements(scope, v, "DOMMatrixReadOnly") {
                    throw_type_error(scope, &format!("{ctx}The provided value is not of type 'DOMMatrixReadOnly'."));
                    return;
                }
                pset(scope, this, "matrix", v);
                return;
            }
            let o = match comp {
                "CSSScale" | "CSSRotate" if k != "angle" => match numberish(scope, v, &ctx) {
                    Some(o) => o,
                    None => return,
                },
                _ => match numeric(scope, v) {
                    Some((_, o)) => o,
                    None => {
                        throw_type_error(scope, &format!("{ctx}The provided value is not of type 'CSSNumericValue'."));
                        return;
                    }
                },
            };
            let (ok, msg) = match (comp, k) {
                ("CSSTranslate", "x") => (type_is(scope, o, "lp"), "Must pass length or percentage to X of CSSTranslate".to_string()),
                ("CSSTranslate", "y") => (type_is(scope, o, "lp"), "Must pass length or percentage to Y of CSSTranslate".to_string()),
                ("CSSTranslate", _) => (type_is(scope, o, "length"), "Must pass length to Z of CSSTranslate".to_string()),
                ("CSSRotate", "angle") => (type_is(scope, o, "angle"), "Must pass an angle to CSSRotate".to_string()),
                ("CSSRotate" | "CSSScale", _) => (type_is(scope, o, "number"), "Must specify a number unit".to_string()),
                ("CSSPerspective", _) => (type_is(scope, o, "length"), "Must pass length or none to CSSPerspective".to_string()),
                (c, _) => (type_is(scope, o, "angle"), format!("{c} does not support non-angles")),
            };
            if !ok {
                throw_type_error(scope, &format!("{ctx}{msg}"));
                return;
            }
            pset(scope, this, k, o.into());
        }
    }
}

fn cb_method<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let (iface, name) = split_data(scope, &args);
    let this = args.this();
    if iface == "CSSNumericValue" && name == "parse" {
        if args.length() < 1 {
            throw_type_error(scope, "Failed to execute 'parse' on 'CSSNumericValue': 1 argument required, but only 0 present.");
            return;
        }
        let Some(t) = args.get(0).to_string(scope) else { return };
        let t = t.to_rust_string_lossy(scope);
        match parse_numeric(scope, &t) {
            Some(o) => rv.set(o.into()),
            None => throw_dom(scope, "Failed to execute 'parse' on 'CSSNumericValue': Invalid math expression", "SyntaxError"),
        }
        return;
    }
    let ok = match iface.as_str() {
        "CSSNumericValue" => kind(scope, this).is_some(),
        "CSSTransformValue" => pstr(scope, this, "list").as_deref() == Some("transform"),
        _ => tkind(scope, this).is_some(),
    };
    if !ok {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    let ctx = format!("Failed to execute '{name}' on '{iface}': ");
    match (iface.as_str(), name.as_str()) {
        ("CSSNumericValue", op @ ("add" | "sub" | "mul" | "div" | "min" | "max")) => {
            if let Some(o) = arith(scope, &args, op) {
                rv.set(o.into());
            }
        }
        ("CSSNumericValue", "equals") => {
            let Some(vals) = numberishes(scope, &args, 0, &ctx) else { return };
            let all = vals.iter().all(|v| equals(scope, this, *v));
            rv.set_bool(all);
        }
        ("CSSNumericValue", "type") => {
            if let Some(t) = ntype(scope, this) {
                let d = type_dict(scope, t);
                rv.set(d.into());
            }
        }
        ("CSSNumericValue", "to") => {
            if args.length() < 1 {
                throw_type_error(scope, &format!("{ctx}1 argument required, but only 0 present."));
                return;
            }
            let Some(s) = args.get(0).to_string(scope) else { return };
            let raw = s.to_rust_string_lossy(scope);
            let Some(target) = unit_from_name(&raw) else {
                throw_dom(scope, &format!("{ctx}Invalid unit for conversion"), "SyntaxError");
                return;
            };
            match to_unit(scope, this, target).and_then(|v| new_unit(scope, v, target)) {
                Some(o) => rv.set(o.into()),
                None => throw_type_error(scope, &format!("{ctx}Cannot convert to {raw}")),
            }
        }
        ("CSSNumericValue", "toSum") => {
            let mut units = Vec::new();
            for i in 0..args.length() {
                let Some(s) = args.get(i).to_string(scope) else { return };
                let raw = s.to_rust_string_lossy(scope);
                let Some(u) = unit_from_name(&raw) else {
                    throw_dom(scope, &format!("{ctx}Invalid unit for conversion"), "SyntaxError");
                    return;
                };
                units.push(u);
            }
            let Some(sum) = sum_value(scope, this) else {
                throw_type_error(scope, &format!("{ctx}Invalid value for conversion"));
                return;
            };
            let mut terms = Vec::new();
            for t in &sum {
                match term_unit(t) {
                    Some(x) => terms.push(Some(x)),
                    None => {
                        throw_type_error(scope, &format!("{ctx}Invalid value for conversion"));
                        return;
                    }
                }
            }
            let mut out = Vec::new();
            if units.is_empty() {
                let mut ts: Vec<(f64, String)> = terms.into_iter().flatten().collect();
                ts.sort_by(|a, b| a.1.cmp(&b.1));
                for (v, u) in ts {
                    let Some(o) = new_unit(scope, v, &u) else { return };
                    out.push(o);
                }
            } else {
                for u in units {
                    let mut total = 0.0;
                    for t in terms.iter_mut() {
                        if let Some((v, tu)) = t {
                            if let Some(c) = convert(*v, tu, u) {
                                total += c;
                                *t = None;
                            }
                        }
                    }
                    let Some(o) = new_unit(scope, total, u) else { return };
                    out.push(o);
                }
                if terms.iter().any(|t| t.is_some()) {
                    throw_type_error(scope, &format!("{ctx}There were leftover terms that were not converted"));
                    return;
                }
            }
            if let Some(o) = create_math(scope, Kind::Sum, &out, &ctx) {
                rv.set(o.into());
            }
        }
        ("CSSTransformValue", "toMatrix") => {
            let Some(m) = new_dommatrix(scope, None) else { return };
            for c in indexed(scope, this) {
                let Ok(c) = v8::Local::<v8::Object>::try_from(c) else { continue };
                let Some(cm) = comp_matrix(scope, c, &ctx) else { return };
                if call_method(scope, m.into(), "multiplySelf", &[cm.into()]).is_none() {
                    return;
                }
            }
            rv.set(m.into());
        }
        ("CSSTransformComponent", "toMatrix") => {
            if let Some(m) = comp_matrix(scope, this, &ctx) {
                rv.set(m.into());
            }
        }
        ("CSSTransformComponent", "toString") => {
            let t = comp_text(scope, this);
            rv.set(str_val(scope, &t));
        }
        _ => {}
    }
}

/// The kit body for the Typed OM rows `gen_shells` claims.
pub fn install_typed_om(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let iface = member.label.split("::").next().unwrap_or("");
    let d = str_val(scope, &format!("{iface}.{}", member.name));
    match member.kind {
        crate::install::MemberKind::Attr { readonly } | crate::install::MemberKind::StaticAttr { readonly } => {
            let g = build_fn_data(scope, cb_get, &format!("get {}", member.name), 0, d);
            let s = if readonly {
                None
            } else {
                let d = str_val(scope, &format!("{iface}.{}", member.name));
                Some(build_fn_data(scope, cb_set, &format!("set {}", member.name), 1, d))
            };
            crate::install::kits::define_accessor(scope, proto, member, g, s);
        }
        _ => {
            let f = build_fn_data(scope, cb_method, member.name, member.length as i32, d);
            crate::install::kits::define_value(scope, proto, member, f.into(), true);
        }
    }
}

// ── the CSS namespace: factories, registerProperty, paintWorklet ──────────────────────────────────

/// `CSS.px(value)` and its 65 siblings: `[NewObject] CSSUnitValue px(double value)`.
pub(crate) fn cb_css_unit(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let name = body_data(scope, &args).to_rust_string_lossy(scope);
    if args.length() < 1 {
        throw_type_error(scope, &format!("Failed to execute '{name}' on 'CSS': 1 argument required, but only 0 present."));
        return;
    }
    let Some(v) = args.get(0).number_value(scope) else { return };
    if !v.is_finite() {
        throw_type_error(scope, &format!("Failed to execute '{name}' on 'CSS': The provided double value is non-finite."));
        return;
    }
    let unit = unit_from_name(&name).unwrap_or("number");
    if let Some(o) = new_unit(scope, v, unit) {
        rv.set(o.into());
    }
}

/// `CSS.registerProperty(definition)` -- Blink's `PropertyRegistration::registerProperty`, into the
/// document's registry (`page::Page::register_property`), so the cascade types the property from then on.
pub(crate) fn cb_css_register_property<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let ctx = "Failed to execute 'registerProperty' on 'CSS': ";
    if args.length() < 1 {
        throw_type_error(scope, &format!("{ctx}1 argument required, but only 0 present."));
        return;
    }
    let a = args.get(0);
    if !a.is_null_or_undefined() && !a.is_object() {
        throw_type_error(scope, &format!("{ctx}The provided value is not of type 'PropertyDefinition'."));
        return;
    }
    let d = v8::Local::<v8::Object>::try_from(a).ok();
    // The dictionary's members, read in lexicographic order: inherits, initialValue, name, syntax.
    fn read<'s>(scope: &mut v8::PinScope<'s, '_>, d: Option<v8::Local<'s, v8::Object>>, k: &str) -> Option<Option<v8::Local<'s, v8::Value>>> {
        match d {
            Some(o) => get_prop(scope, o, k).map(|v| if v.is_undefined() { None } else { Some(v) }),
            None => Some(None),
        }
    }
    let Some(inherits) = read(scope, d, "inherits") else { return };
    let Some(inherits) = inherits else {
        throw_type_error(scope, &format!("{ctx}Failed to read the 'inherits' property from 'PropertyDefinition': Required member is undefined."));
        return;
    };
    let inherits = inherits.boolean_value(scope);
    let Some(initial) = read(scope, d, "initialValue") else { return };
    let initial = match initial {
        Some(v) => match v.to_string(scope) {
            Some(s) => Some(s.to_rust_string_lossy(scope)),
            None => return,
        },
        None => None,
    };
    let Some(name) = read(scope, d, "name") else { return };
    let Some(name) = name else {
        throw_type_error(scope, &format!("{ctx}Failed to read the 'name' property from 'PropertyDefinition': Required member is undefined."));
        return;
    };
    let Some(name) = name.to_string(scope) else { return };
    let name = name.to_rust_string_lossy(scope);
    let Some(syntax) = read(scope, d, "syntax") else { return };
    let syntax = match syntax {
        Some(v) => match v.to_string(scope) {
            Some(s) => s.to_rust_string_lossy(scope),
            None => return,
        },
        None => "*".to_string(),
    };
    if !is_custom_property_name(&name) {
        throw_dom(scope, &format!("{ctx}Custom property names must start with '--'."), "SyntaxError");
        return;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if crate::core::css::cssom::is_script_registered(&op_state.borrow(), &name) {
        throw_dom(scope, &format!("{ctx}The name provided has already been registered."), "InvalidModificationError");
        return;
    }
    use page::style::registered::{RegisterError, Registration};
    match Registration::from_script(&syntax, inherits, initial.as_deref()) {
        Ok(reg) => {
            crate::core::css::cssom::register_property(&op_state.borrow(), &name, reg);
        }
        Err(e) => {
            let msg = match e {
                RegisterError::BadSyntax => "The syntax provided is not a valid custom property syntax.",
                RegisterError::NoInitial => "An initial value must be provided if the syntax is not '*'",
                RegisterError::InitialDoesNotParse => "The initial value provided does not parse for the given syntax.",
                RegisterError::NotIndependent => "The initial value provided is not computationally independent.",
            };
            throw_dom(scope, &format!("{ctx}{msg}"), "SyntaxError");
        }
    }
}

const PAINT_WORKLET_SLOT: &str = "paintWorklet";

/// `CSS.paintWorklet`: the realm's one `Worklet` (`[SameObject]`), an accessor on the namespace like
/// `CSS.highlights`.
fn cb_css_paint_worklet(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let global = scope.get_current_context().global(scope);
    if let Some(w) = pget(scope, global, PAINT_WORKLET_SLOT) {
        rv.set(w);
        return;
    }
    let Some(w) = mint(scope, "Worklet") else { return };
    pset(scope, global, PAINT_WORKLET_SLOT, w.into());
    rv.set(w.into());
}

/// The `CSS` namespace's Typed OM half, in Chrome 150's own-property order: the factories and
/// `registerProperty` sorted in among `escape` / `supports` (the table's order), `highlights` first
/// and `paintWorklet` last (`install::live::css`).
pub(crate) fn install_paint_worklet(scope: &mut v8::PinScope, css: v8::Local<v8::Object>) {
    let g = crate::binding::protos::build_fn(scope, cb_css_paint_worklet, "get paintWorklet", 0);
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let mut d = v8::PropertyDescriptor::new_from_get_set(g.into(), undef);
    d.set_enumerable(true);
    d.set_configurable(true);
    if let Some(k) = v8::String::new(scope, "paintWorklet") {
        css.define_property(scope, k.into(), &d);
    }
}

// ── reification (what StylePropertyMap.get / CSSStyleValue.parse hand back) ────────────────────────

/// A value's Typed OM object, as Blink's `StyleValueFactory` makes it for the shapes this engine
/// reifies: a lone number / percentage / dimension is a `CSSUnitValue`, a lone keyword a
/// `CSSKeywordValue`, a lone `calc()`-family function its math value, a custom property's value a
/// `CSSUnparsedValue`. `None` = some other shape (the caller's generic `CSSStyleValue`).
pub(crate) fn reify<'s>(scope: &mut v8::PinScope<'s, '_>, prop: &str, text: &str) -> Option<v8::Local<'s, v8::Object>> {
    if prop.starts_with("--") {
        let o = mint(scope, "CSSUnparsedValue")?;
        pset_str(scope, o, "list", "unparsed");
        let s = str_val(scope, text);
        define_index(scope, o, 0, s, true);
        return Some(o);
    }
    // Every other property's name is ASCII case-insensitive (CSS Typed OM: "if property is not a custom property
    // name string, set property to property ASCII lowercased"). The gates below are lower-case tables, so `get('wIdTh')`
    // fell through them to `CSSStyleValue` (WPT `{inline,computed,declared}/get{,All}` "not case-sensitive", which
    // Chrome passes; pinned by the `typedom_types` gate).
    let lower = prop.to_ascii_lowercase();
    let prop = lower.as_str();
    // A shorthand is never reified: `GetShorthandProperty` hands back a `CSSUnsupportedStyleValue` over its
    // serialization (Chrome 150: `margin-block: initial` reads `CSSStyleValue`).
    if page::style::shorthand::is_shorthand(prop) {
        return None;
    }
    // `CreateStyleValueWithoutProperty`, Blink's FIRST step for every longhand, supported or not: a CSS-wide keyword is
    // a `CSSKeywordValue` (as Blink serializes it, lower-case). The `typedom_types` gate once ran before it, and `color: initial`,
    // `background-repeat: inherit`, `border-block-start-width: unset` read `CSSStyleValue` (WPT typed-OM rows Chrome
    // passes).
    let t = text.trim();
    if CSS_WIDE_KEYWORDS.iter().any(|k| t.eq_ignore_ascii_case(k)) {
        let o = mint(scope, "CSSKeywordValue")?;
        pset_str(scope, o, "kw", &t.to_ascii_lowercase());
        return Some(o);
    }
    // `CreateStyleValueWithProperty`: a property Typed OM does not support (no `typedom_types`) is a plain
    // `CSSStyleValue` whatever its value, and a colour property reifies only `currentcolor` (`auto` too for
    // accent-color / caret-color) -- the rest of Blink's per-property switch needs a list or a pair, which a lone
    // token never is. Chrome 150 (`F-attribute-style-map`, three runs alike): `attributeStyleMap.get('color')` for
    // `color: red` is `[object CSSStyleValue]`; korvus made every lone keyword a `CSSKeywordValue`.
    if TYPEDOM_PROPERTIES.binary_search(&prop).is_err() {
        return None;
    }
    if COLOR_PROPERTIES.contains(&prop) {
        let t = text.trim();
        let keyword = t.eq_ignore_ascii_case("currentcolor") || (matches!(prop, "accent-color" | "caret-color") && t.eq_ignore_ascii_case("auto"));
        if !keyword {
            return None;
        }
    }
    let mut input = cssparser::ParserInput::new(text);
    let mut p = cssparser::Parser::new(&mut input);
    let tok = p.next().ok()?.clone();
    if !p.is_exhausted() {
        return None;
    }
    match tok {
        cssparser::Token::Ident(k) => {
            let o = mint(scope, "CSSKeywordValue")?;
            pset_str(scope, o, "kw", &k);
            Some(o)
        }
        cssparser::Token::Number { .. } | cssparser::Token::Percentage { .. } | cssparser::Token::Dimension { .. } | cssparser::Token::Function(_) => {
            parse_numeric(scope, text.trim())
        }
        _ => None,
    }
}

/// The CSS-wide keywords (`css_parsing_utils::IsCSSWideKeyword`, Chromium 150; `revert-rule` is behind a runtime flag
/// that stable Chrome keeps off).
const CSS_WIDE_KEYWORDS: [&str; 5] = ["initial", "inherit", "unset", "revert", "revert-layer"];

/// The properties Typed OM supports (`CSSOMTypes::IsPropertySupported`): those with `typedom_types` in Chromium
/// 150's `css_properties.json5`, sorted for `binary_search`.
const TYPEDOM_PROPERTIES: &[&str] = &[
    "-internal-overscroll-area", "-internal-overscroll-position", "accent-color", "alignment-baseline", "anchor-name",
    "anchor-scope", "animation-composition", "animation-delay", "animation-direction", "animation-duration",
    "animation-fill-mode", "animation-iteration-count", "animation-name", "animation-play-state",
    "animation-timeline", "animation-timing-function", "backdrop-filter", "backface-visibility",
    "background-attachment", "background-blend-mode", "background-clip", "background-color", "background-image",
    "background-origin", "background-size", "baseline-shift", "baseline-source", "block-ellipsis", "block-size",
    "border-bottom-color", "border-bottom-left-radius", "border-bottom-right-radius", "border-bottom-style",
    "border-bottom-width", "border-collapse", "border-end-end-radius", "border-end-start-radius",
    "border-image-outset", "border-image-repeat", "border-image-slice", "border-image-source", "border-image-width",
    "border-left-color", "border-left-style", "border-left-width", "border-right-color", "border-right-style",
    "border-right-width", "border-shape", "border-start-end-radius", "border-start-start-radius", "border-top-color",
    "border-top-left-radius", "border-top-right-radius", "border-top-style", "border-top-width", "bottom",
    "box-shadow", "box-sizing", "break-after", "break-before", "break-inside", "caption-side", "caret-animation",
    "caret-color", "caret-shape", "clear", "clip", "clip-path", "clip-rule", "color", "color-interpolation",
    "color-interpolation-filters", "color-rendering", "column-count", "column-fill", "column-gap", "column-height",
    "column-rule-break", "column-rule-color", "column-rule-inset-cap-end", "column-rule-inset-cap-start",
    "column-rule-inset-junction-end", "column-rule-inset-junction-start", "column-rule-style",
    "column-rule-visibility-items", "column-rule-width", "column-span", "column-width", "column-wrap", "contain",
    "contain-intrinsic-block-size", "contain-intrinsic-inline-size", "container-name", "container-type",
    "content-visibility", "continue", "counter-increment", "counter-reset", "counter-set", "cursor", "cx", "cy", "d",
    "direction", "display", "dominant-baseline", "dynamic-range-limit", "empty-cells", "field-sizing", "fill-opacity",
    "fill-rule", "filter", "flex-basis", "flex-direction", "flex-grow", "flex-line-count", "flex-shrink", "flex-wrap",
    "float", "flood-color", "flood-opacity", "flow-tolerance", "font-feature-settings", "font-kerning",
    "font-language-override", "font-optical-sizing", "font-palette", "font-size", "font-size-adjust", "font-stretch",
    "font-style", "font-synthesis-small-caps", "font-synthesis-style", "font-synthesis-weight",
    "font-variant-alternates", "font-variant-caps", "font-variant-east-asian", "font-variant-emoji",
    "font-variant-ligatures", "font-variant-numeric", "font-variant-position", "font-variation-settings",
    "font-weight", "forced-color-adjust", "frame-sizing", "grid-auto-columns", "grid-auto-flow", "grid-auto-rows",
    "grid-column-end", "grid-column-start", "grid-lanes-direction", "grid-lanes-pack", "grid-row-end",
    "grid-row-start", "grid-template-areas", "grid-template-columns", "grid-template-rows", "hanging-punctuation",
    "height", "hyphens", "image-animation", "image-rendering", "inline-size", "inset-block-end", "inset-block-start",
    "inset-inline-end", "inset-inline-start", "interactivity", "interpolate-size", "isolation", "left",
    "letter-spacing", "lighting-color", "line-break", "line-height", "list-style-image", "list-style-position",
    "margin-block-end", "margin-block-start", "margin-bottom", "margin-inline-end", "margin-inline-start",
    "margin-left", "margin-right", "margin-top", "margin-trim", "marker-end", "marker-mid", "marker-start",
    "mask-type", "math-depth", "math-shift", "math-style", "max-block-size", "max-height", "max-inline-size",
    "max-lines", "max-width", "min-block-size", "min-height", "min-inline-size", "min-width", "mix-blend-mode",
    "object-fit", "object-position", "object-view-box", "offset-anchor", "offset-distance", "offset-path",
    "offset-position", "offset-rotate", "opacity", "order", "origin-trial-test-property", "orphans", "outline-color",
    "outline-offset", "outline-style", "outline-width", "overflow-anchor", "overflow-wrap", "overflow-x",
    "overflow-y", "overlay", "overscroll-behavior-x", "overscroll-behavior-y", "padding-block-end",
    "padding-block-start", "padding-bottom", "padding-inline-end", "padding-inline-start", "padding-left",
    "padding-right", "padding-top", "page", "paint-order", "path-length", "perspective", "perspective-origin",
    "pointer-events", "position", "position-anchor", "position-area", "position-try-fallbacks", "position-try-order",
    "position-visibility", "quotes", "r", "reading-flow", "reading-order", "resize", "right", "row-gap",
    "row-rule-break", "row-rule-color", "row-rule-inset-cap-end", "row-rule-inset-cap-start",
    "row-rule-inset-junction-end", "row-rule-inset-junction-start", "row-rule-style", "row-rule-visibility-items",
    "row-rule-width", "rule-overlap", "rx", "ry", "scroll-axis-lock", "scroll-behavior", "scroll-margin-block-end",
    "scroll-margin-block-start", "scroll-margin-bottom", "scroll-margin-inline-end", "scroll-margin-inline-start",
    "scroll-margin-left", "scroll-margin-right", "scroll-margin-top", "scroll-marker-group",
    "scroll-padding-block-end", "scroll-padding-block-start", "scroll-padding-bottom", "scroll-padding-inline-end",
    "scroll-padding-inline-start", "scroll-padding-left", "scroll-padding-right", "scroll-padding-top",
    "scroll-snap-align", "scroll-snap-stop", "scroll-snap-type", "scroll-target-group", "scrollbar-gutter",
    "scrollbar-width", "shape-image-threshold", "shape-margin", "shape-outside", "shape-rendering", "stop-color",
    "stop-opacity", "stroke-dasharray", "stroke-dashoffset", "stroke-linecap", "stroke-linejoin", "stroke-miterlimit",
    "stroke-opacity", "stroke-width", "tab-size", "table-layout", "text-align", "text-align-last", "text-anchor",
    "text-autospace", "text-box-trim", "text-combine-upright", "text-decoration-color", "text-decoration-line",
    "text-decoration-skip-ink", "text-decoration-skip-spaces", "text-decoration-style", "text-decoration-thickness",
    "text-emphasis-color", "text-indent", "text-justify", "text-orientation", "text-overflow", "text-rendering",
    "text-shadow", "text-size-adjust", "text-spacing-trim", "text-transform", "text-underline-offset",
    "text-underline-position", "text-wrap-mode", "text-wrap-style", "timeline-trigger-source", "top", "touch-action",
    "transform", "transform-box", "transform-style", "transition-behavior", "transition-delay", "transition-duration",
    "transition-property", "transition-timing-function", "trigger-scope", "unicode-bidi", "user-select",
    "vector-effect", "vertical-align", "view-transition-class", "view-transition-group", "view-transition-name",
    "view-transition-scope", "visibility", "white-space-collapse", "widows", "width", "will-change", "word-break",
    "word-spacing", "writing-mode", "x", "y", "z-index",
];

/// `CreateStyleValueWithPropertyInternal`'s colour cases: only `currentcolor` is a keyword.
const COLOR_PROPERTIES: &[&str] = &[
    "accent-color", "background-color", "border-bottom-color", "border-left-color", "border-right-color", "border-top-color",
    "caret-color", "color", "column-rule-color", "flood-color", "lighting-color", "outline-color", "stop-color",
    "text-decoration-color", "text-emphasis-color",
];

/// What a value given to `StylePropertyMap.set` / `append` writes: a Typed OM value's text, a string
/// as it is. `None` = not a value of any kind (a TypeError the caller raises).
pub(crate) fn value_text(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<String> {
    if let Ok(o) = v8::Local::<v8::Object>::try_from(v) {
        if let Some(t) = style_value_text(scope, o) {
            return Some(t);
        }
        if crate::binding::protos::implements(scope, v, "CSSStyleValue") {
            return call_method(scope, v, "toString", &[]).map(|s| s.to_rust_string_lossy(scope));
        }
    }
    if v.is_string() {
        return Some(v.to_rust_string_lossy(scope));
    }
    None
}
