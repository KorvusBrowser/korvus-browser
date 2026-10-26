//! `SVGTransform` and an element's `transform` list as Blink keeps them.
//!
//! A transform is an OBJECT -- its type, its matrix, its angle (`core/svg/svg_transform.cc`) -- and an element's
//! `transform` list holds transforms, not text: `getItem` hands out tear-offs of the same object, a tear-off's
//! `setScale` changes that object and writes the list back to the attribute (`CommitChange`), and a detached
//! transform appended to a list IS the list's item from then on, so changing it later changes the list. The
//! attribute is the list's serialization (`SVGTransform::ValueAsString`, numbers to six significant digits:
//! `scale(2)` reads back `scale(2 2)` once the list is written), and a write of the attribute -- the same text
//! included -- parses it into new items (a parse error leaves the list empty).
//!
//! This answered `replaceItem` and the six setters with nothing (their rows were stubs) and read every item as
//! text of the attribute: WPT `svg/coordinate-systems/svgtransformlist-replaceitem.html` replaces a text's
//! `scale(1)` with `scale(10 1)` and compares its box with one written so -- it held only while a text's box
//! ignored its scale. Every answer here was read off stock Chrome 150 (`tests/fixtures/svg-text/transform-*`).
//!
//! Items live in a per-realm store by id; a tear-off carries its item's id, the element whose list it was handed
//! out from (Blink's context element -- a write through it commits to that element) and whether it is read-only
//! (an `animVal` item). The store keeps what it made: Blink's items are garbage collected, these are not -- a page
//! makes them only through the list API (`createSVGTransform`, a list read after the attribute changed).

use deno_core::{v8, OpState};
use std::collections::HashMap;

use super::{mark_read_only, mint, slot_get, slot_set, SLOT_RO};
use crate::binding::protos::{throw_dom, throw_type_error};
use crate::binding::HostDom;

// ── Blink's SVGTransform ────────────────────────────────────────────────────────────────────────────

/// `SVGTransformType`.
const T_MATRIX: u16 = 1;
const T_TRANSLATE: u16 = 2;
const T_SCALE: u16 = 3;
const T_ROTATE: u16 = 4;
const T_SKEWX: u16 = 5;
const T_SKEWY: u16 = 6;

const IDENTITY: [f64; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// One transform: its type, its matrix `[a b c d e f]` (doubles, as `AffineTransform` keeps them), its angle.
#[derive(Clone, Copy, Debug)]
pub(super) struct Tx {
    kind: u16,
    m: [f64; 6],
    angle: f32,
}

/// `Deg2rad(double)`.
fn deg2rad(d: f64) -> f64 {
    d * (std::f64::consts::PI / 180.0)
}

/// `AffineTransform::Translate`.
fn translate(m: &mut [f64; 6], tx: f64, ty: f64) {
    m[4] += tx * m[0] + ty * m[2];
    m[5] += tx * m[1] + ty * m[3];
}

/// `DoMultiply(t1, t2)`: `t1 * t2`, with Blink's shortcut for a `t1` that only translates.
fn multiply(t1: [f64; 6], t2: [f64; 6]) -> [f64; 6] {
    if t1[0] == 1.0 && t1[1] == 0.0 && t1[2] == 0.0 && t1[3] == 1.0 {
        return [t2[0], t2[1], t2[2], t2[3], t1[4] + t2[4], t1[5] + t2[5]];
    }
    [
        t1[0] * t2[0] + t1[2] * t2[1],
        t1[1] * t2[0] + t1[3] * t2[1],
        t1[0] * t2[2] + t1[2] * t2[3],
        t1[1] * t2[2] + t1[3] * t2[3],
        t1[0] * t2[4] + t1[2] * t2[5] + t1[4],
        t1[1] * t2[4] + t1[3] * t2[5] + t1[5],
    ]
}

impl Tx {
    /// `SVGTransform::SetMatrix` / a parsed `matrix(...)` / `createSVGTransform()` (the identity).
    fn matrix(m: [f64; 6]) -> Tx {
        Tx { kind: T_MATRIX, m, angle: 0.0 }
    }

    /// `SetTranslate`: the identity translated.
    fn translation(tx: f32, ty: f32) -> Tx {
        let mut m = IDENTITY;
        translate(&mut m, tx as f64, ty as f64);
        Tx { kind: T_TRANSLATE, m, angle: 0.0 }
    }

    /// `SetScale`: the identity scaled column by column (`AffineTransform::Scale`).
    fn scaling(sx: f32, sy: f32) -> Tx {
        let (sx, sy) = (sx as f64, sy as f64);
        let mut m = IDENTITY;
        m[0] *= sx;
        m[1] *= sx;
        m[2] *= sy;
        m[3] *= sy;
        Tx { kind: T_SCALE, m, angle: 0.0 }
    }

    /// `SetRotate` / `MakeRotationAroundPoint`: translate to the centre, rotate, translate back.
    fn rotation(angle: f32, cx: f32, cy: f32) -> Tx {
        let mut m = IDENTITY;
        translate(&mut m, cx as f64, cy as f64);
        let r = deg2rad(angle as f64);
        let (s, c) = (r.sin(), r.cos());
        m = multiply(m, [c, s, -s, c, 0.0, 0.0]);
        translate(&mut m, -(cx as f64), -(cy as f64));
        Tx { kind: T_ROTATE, m, angle }
    }

    /// `SetSkewX` / `SetSkewY` (`MakeSkewX` / `MakeSkewY`).
    fn skew(kind: u16, angle: f32) -> Tx {
        let t = deg2rad(angle as f64).tan();
        let m = if kind == T_SKEWX { [1.0, 0.0, t, 1.0, 0.0, 0.0] } else { [1.0, t, 0.0, 1.0, 0.0, 0.0] };
        Tx { kind, m, angle }
    }

    /// `SVGTransform::ValueAsString`.
    fn value_as_string(&self) -> String {
        let (prefix, args): (&str, Vec<f64>) = match self.kind {
            T_MATRIX => ("matrix(", self.m.to_vec()),
            T_TRANSLATE => ("translate(", vec![self.m[4], self.m[5]]),
            T_SCALE => ("scale(", vec![self.m[0], self.m[3]]),
            T_ROTATE => {
                let mut a = vec![self.angle as f64];
                let (cx, cy) = rotation_center(&self.m, self.angle);
                if cx != 0.0 || cy != 0.0 {
                    a.push(cx as f64);
                    a.push(cy as f64);
                }
                ("rotate(", a)
            }
            T_SKEWX => ("skewX(", vec![self.angle as f64]),
            T_SKEWY => ("skewY(", vec![self.angle as f64]),
            _ => return String::new(),
        };
        let body: Vec<String> = args.iter().map(|v| blink_number(*v)).collect();
        format!("{prefix}{})", body.join(" "))
    }
}

/// `DecomposeRotationCenter`: the centre a rotation matrix turns about, from its translation. The angle goes to
/// radians in FLOAT (`Deg2rad(float)`), the rest in double, the result clamped to float.
fn rotation_center(m: &[f64; 6], angle: f32) -> (f32, f32) {
    let rad = (angle * (std::f32::consts::PI / 180.0)) as f64;
    let (c, s) = (rad.cos(), rad.sin());
    if c == 1.0 {
        return (0.0, 0.0);
    }
    let (e, f) = (m[4], m[5]);
    let cx = (e * (1.0 - c) - f * s) / (1.0 - c) / 2.0;
    let cy = (e * s / (1.0 - c) + f) / 2.0;
    (clamp_f32(cx), clamp_f32(cy))
}

/// `ClampTo<float>`.
fn clamp_f32(d: f64) -> f32 {
    if d >= f32::MAX as f64 {
        f32::MAX
    } else if d <= f32::MIN as f64 {
        f32::MIN
    } else {
        d as f32
    }
}

/// `StringBuilder::AppendNumber(double)`: ECMAScript's `toPrecision(6)` -- exponential below 1e-6 and from 1e6 --
/// with its trailing zeros dropped only when there is no exponent (`1.00000e-7`, `0.1`, `1.23457e+6`).
fn blink_number(d: f64) -> String {
    if d.is_nan() {
        return "NaN".into();
    }
    if d.is_infinite() {
        return if d > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if d == 0.0 {
        return "0".into();
    }
    let (digits, e) = six_digits(d.abs());
    let mut s = String::new();
    if d < 0.0 {
        s.push('-');
    }
    let dig = |i: usize| (b'0' + digits[i]) as char;
    if !(-6..6).contains(&e) {
        s.push(dig(0));
        s.push('.');
        (1..6).for_each(|i| s.push(dig(i)));
        s.push('e');
        s.push(if e >= 0 { '+' } else { '-' });
        s.push_str(&e.abs().to_string());
        return s;
    }
    let mut int = String::new();
    let mut frac = String::new();
    if e >= 0 {
        (0..=e as usize).for_each(|i| int.push(dig(i)));
        (e as usize + 1..6).for_each(|i| frac.push(dig(i)));
    } else {
        int.push('0');
        (0..(-e - 1)).for_each(|_| frac.push('0'));
        (0..6).for_each(|i| frac.push(dig(i)));
    }
    let frac = frac.trim_end_matches('0');
    s.push_str(&int);
    if !frac.is_empty() {
        s.push('.');
        s.push_str(frac);
    }
    s
}

/// `x`'s first six significant digits, rounded as `toPrecision` rounds (a tie goes up), and its decimal exponent.
fn six_digits(x: f64) -> ([u8; 6], i32) {
    let s = format!("{:.40e}", x);
    let (m, e) = s.split_once('e').unwrap_or((&s, "0"));
    let mut exp: i32 = e.parse().unwrap_or(0);
    let all: Vec<u8> = m.bytes().filter(u8::is_ascii_digit).map(|b| b - b'0').collect();
    let mut out = [0u8; 6];
    out.copy_from_slice(&all[..6]);
    if all.get(6).is_some_and(|&d| d >= 5) {
        let mut i = 6;
        loop {
            if i == 0 {
                out = [1, 0, 0, 0, 0, 0];
                exp += 1;
                break;
            }
            i -= 1;
            if out[i] == 9 {
                out[i] = 0;
            } else {
                out[i] += 1;
                break;
            }
        }
    }
    (out, exp)
}

// ── the attribute's grammar (`SVGTransformList::ParseInternal`) ─────────────────────────────────

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0c)
}

/// `SkipOptionalSVGSpaces`: past the spaces; whether anything is left.
fn skip_spaces(s: &[u8], p: &mut usize) -> bool {
    while *p < s.len() && is_space(s[*p]) {
        *p += 1;
    }
    *p < s.len()
}

/// `GenericParseNumber<float>` with leading spaces allowed: Blink's own accumulation, digit by digit in float
/// (not a correctly rounded parse -- `1.23456789` is the float `1.2345678806304932` there, measured).
fn parse_number(s: &[u8], pos: &mut usize) -> Option<f32> {
    let valid = |x: f32| x.is_finite();
    let mut p = *pos;
    skip_spaces(s, &mut p);
    let mut sign = 1.0f32;
    if p < s.len() && s[p] == b'+' {
        p += 1;
    } else if p < s.len() && s[p] == b'-' {
        p += 1;
        sign = -1.0;
    }
    if p >= s.len() || (!s[p].is_ascii_digit() && s[p] != b'.') {
        return None;
    }
    let start = p;
    while p < s.len() && s[p].is_ascii_digit() {
        p += 1;
    }
    let mut integer = 0.0f32;
    if p > start {
        let mut multiplier = 1.0f32;
        let mut i = p;
        while i > start {
            integer += multiplier * (s[i - 1] - b'0') as f32;
            multiplier *= 10.0;
            i -= 1;
        }
        if !valid(integer) {
            return None;
        }
    }
    let mut decimal = 0.0f32;
    if p < s.len() && s[p] == b'.' {
        p += 1;
        if p >= s.len() || !s[p].is_ascii_digit() {
            return None;
        }
        let mut frac = 1.0f32;
        while p < s.len() && s[p].is_ascii_digit() {
            frac *= 0.1f32;
            decimal += (s[p] - b'0') as f32 * frac;
            p += 1;
        }
    }
    let mut number = (integer + decimal) * sign;
    if s.len() - p > 1 && (s[p] == b'e' || s[p] == b'E') && s[p + 1] != b'x' && s[p + 1] != b'm' {
        p += 1;
        let mut negative = false;
        if s[p] == b'+' {
            p += 1;
        } else if s[p] == b'-' {
            p += 1;
            negative = true;
        }
        if p >= s.len() || !s[p].is_ascii_digit() {
            return None;
        }
        let mut exponent = 0.0f32;
        while p < s.len() && s[p].is_ascii_digit() {
            exponent *= 10.0;
            exponent += (s[p] - b'0') as f32;
            p += 1;
        }
        if negative {
            exponent = -exponent;
        }
        if exponent > f32::MAX_10_EXP as f32 {
            return None;
        }
        if exponent != 0.0 {
            number *= 10f64.powf(exponent as f64) as f32;
        }
    }
    if !valid(number) {
        return None;
    }
    *pos = p;
    Some(number)
}

/// A transform list's text as Blink parses it -- `None` at the first error, where Blink leaves the list EMPTY
/// (`SetValueAsString` clears what it parsed).
fn parse_list(text: &str) -> Option<Vec<Tx>> {
    let s = text.as_bytes();
    let mut p = 0usize;
    let mut out = Vec::new();
    let mut delim = false;
    while skip_spaces(s, &mut p) {
        delim = false;
        let rest = &s[p..];
        let (kind, name): (u16, &[u8]) = if rest.starts_with(b"skewX") {
            (T_SKEWX, b"skewX")
        } else if rest.starts_with(b"skewY") {
            (T_SKEWY, b"skewY")
        } else if rest.starts_with(b"scale") {
            (T_SCALE, b"scale")
        } else if rest.starts_with(b"translate") {
            (T_TRANSLATE, b"translate")
        } else if rest.starts_with(b"rotate") {
            (T_ROTATE, b"rotate")
        } else if rest.starts_with(b"matrix") {
            (T_MATRIX, b"matrix")
        } else {
            return None;
        };
        p += name.len();
        if !skip_spaces(s, &mut p) || s[p] != b'(' {
            return None;
        }
        p += 1;
        // kRequiredValuesForType / kOptionalValuesForType
        let (required, optional) = match kind {
            T_MATRIX => (6, 0),
            T_TRANSLATE | T_SCALE => (1, 1),
            T_ROTATE => (1, 2),
            _ => (1, 0),
        };
        let mut args: Vec<f32> = Vec::new();
        let mut trailing = false;
        while args.len() < required + optional {
            let Some(v) = parse_number(s, &mut p) else { break };
            args.push(v);
            trailing = false;
            if args.len() == required + optional {
                break;
            }
            if skip_spaces(s, &mut p) && s[p] == b',' {
                p += 1;
                trailing = true;
            }
        }
        if (args.len() != required && args.len() != required + optional) || trailing {
            return None;
        }
        if !skip_spaces(s, &mut p) || s[p] != b')' {
            return None;
        }
        p += 1;
        out.push(match kind {
            T_SKEWX | T_SKEWY => Tx::skew(kind, args[0]),
            T_SCALE => Tx::scaling(args[0], *args.get(1).unwrap_or(&args[0])),
            T_TRANSLATE => Tx::translation(args[0], *args.get(1).unwrap_or(&0.0)),
            T_ROTATE if args.len() == 1 => Tx::rotation(args[0], 0.0, 0.0),
            T_ROTATE => Tx::rotation(args[0], args[1], args[2]),
            _ => Tx::matrix([args[0] as f64, args[1] as f64, args[2] as f64, args[3] as f64, args[4] as f64, args[5] as f64]),
        });
        if skip_spaces(s, &mut p) && s[p] == b',' {
            p += 1;
            delim = true;
        }
    }
    if delim {
        return None;
    }
    Some(out)
}

// ── the store ───────────────────────────────────────────────────────────────────────────────────

/// Every transform the realm's lists and `createSVGTransform` made, by id, with the element whose list holds
/// it (Blink's `OwnerList`), and each element's list as it was last read: the `transform` attribute's write
/// count then, and the items.
#[derive(Default)]
struct TxStore {
    items: Vec<Tx>,
    owner: Vec<Option<u32>>,
    lists: HashMap<u32, (u32, Vec<u32>)>,
}

fn with_store<R>(st: &mut OpState, f: impl FnOnce(&mut TxStore) -> R) -> R {
    if !st.has::<TxStore>() {
        st.put(TxStore::default());
    }
    f(st.borrow_mut::<TxStore>())
}

fn new_item(st: &mut OpState, tx: Tx, owner: Option<u32>) -> u32 {
    with_store(st, |s| {
        s.items.push(tx);
        s.owner.push(owner);
        (s.items.len() - 1) as u32
    })
}

fn item(st: &mut OpState, id: u32) -> Tx {
    with_store(st, |s| s.items.get(id as usize).copied().unwrap_or(Tx::matrix(IDENTITY)))
}

fn set_item(st: &mut OpState, id: u32, tx: Tx) {
    with_store(st, |s| {
        if let Some(t) = s.items.get_mut(id as usize) {
            *t = tx;
        }
    })
}

/// Element `el`'s list, parsed again from its attribute when that was written since it was last read -- its
/// old items leave it.
fn list_ids(st: &mut OpState, el: u32) -> Vec<u32> {
    let (writes, text) = match st.try_borrow::<HostDom>() {
        Some(h) => (h.0.transform_writes(el), h.0.get_attribute(el, "transform").map(str::to_string)),
        None => return Vec::new(),
    };
    with_store(st, |s| {
        if let Some((w, ids)) = s.lists.get(&el) {
            if *w == writes {
                return ids.clone();
            }
        }
        if let Some((_, old)) = s.lists.get(&el) {
            for &i in old {
                s.owner[i as usize] = None;
            }
        }
        let parsed = text.as_deref().and_then(parse_list).unwrap_or_default();
        let ids: Vec<u32> = parsed
            .into_iter()
            .map(|t| {
                s.items.push(t);
                s.owner.push(Some(el));
                (s.items.len() - 1) as u32
            })
            .collect();
        s.lists.insert(el, (writes, ids.clone()));
        ids
    })
}

/// Replace element `el`'s list (after [`list_ids`]): the items that leave it are detached, the new ones its own.
fn set_list(st: &mut OpState, el: u32, ids: Vec<u32>) {
    with_store(st, |s| {
        let writes = s.lists.get(&el).map(|(w, _)| *w).unwrap_or(0);
        if let Some((_, old)) = s.lists.get(&el) {
            for &i in old {
                if !ids.contains(&i) {
                    s.owner[i as usize] = None;
                }
            }
        }
        for &i in &ids {
            s.owner[i as usize] = Some(el);
        }
        s.lists.insert(el, (writes, ids));
    })
}

/// Blink's `CommitChange` for element `el`'s list: the attribute becomes the list's serialization, and an empty
/// list removes it (Chrome 150: `clear()` and removing the last item leave no `transform` attribute).
fn commit(scope: &mut v8::PinScope, el: u32) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let text = {
        let mut st = op_state.borrow_mut();
        let ids = list_ids(&mut st, el);
        let parts: Vec<String> = ids.iter().map(|&i| item(&mut st, i).value_as_string()).collect();
        (!ids.is_empty()).then(|| parts.join(" "))
    };
    match text {
        Some(t) => crate::core::dom::shared::set_attribute_notify(scope, &op_state, el, "transform", &t),
        None => crate::core::dom::shared::remove_attribute_notify(scope, &op_state, el, "transform"),
    }
    // the list stays as it is: what the attribute now says is what it holds
    let mut st = op_state.borrow_mut();
    let writes = st.try_borrow::<HostDom>().map(|h| h.0.transform_writes(el)).unwrap_or(0);
    with_store(&mut st, |s| {
        if let Some(l) = s.lists.get_mut(&el) {
            l.0 = writes;
        }
    });
}

// ── the tear-offs ───────────────────────────────────────────────────────────────────────────────

/// The item id a transform tear-off stands for.
const SLOT_TX: &str = "korvus_svg_tx";
/// The element a tear-off commits to (Blink's context element): the list's it was handed out from, or the one a
/// detached transform was inserted into.
const SLOT_TX_EL: &str = "korvus_svg_tx_el";
/// On a transform's `matrix`: the transform tear-off it belongs to.
const SLOT_TX_OF: &str = "korvus_svg_tx_of";

fn num_slot(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, name: &str) -> Option<u32> {
    slot_get(scope, o, name).and_then(|v| v.number_value(scope)).map(|n| n as u32)
}

fn set_num_slot(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, name: &str, n: u32) {
    let v = v8::Number::new(scope, n as f64);
    slot_set(scope, o, name, v.into());
}

/// The item a transform tear-off stands for.
pub(super) fn tx_of(scope: &mut v8::PinScope, o: v8::Local<v8::Object>) -> Option<u32> {
    num_slot(scope, o, SLOT_TX)
}

fn is_read_only(scope: &mut v8::PinScope, o: v8::Local<v8::Object>) -> bool {
    slot_get(scope, o, SLOT_RO).is_some()
}

/// A new tear-off of item `id`: bound to element `el`'s list, or detached; read-only for an `animVal` item.
fn tear_off<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32, el: Option<u32>, read_only: bool) -> Option<v8::Local<'s, v8::Object>> {
    let o = mint(scope, "SVGTransform", None, None)?;
    set_num_slot(scope, o, SLOT_TX, id);
    if let Some(e) = el {
        set_num_slot(scope, o, SLOT_TX_EL, e);
    }
    if read_only {
        mark_read_only(scope, o);
    }
    Some(o)
}

/// `svg.createSVGTransform()`: a detached identity `matrix` transform.
pub(super) fn create<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    create_from(scope, IDENTITY)
}

/// `createSVGTransformFromMatrix(matrix)` on `iface` (an SVG root or a transform list): a detached `matrix`
/// transform holding an SVGMatrix's values, or Blink's TypeError for anything else.
pub(super) fn from_matrix<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<v8::Value>, iface: &str) -> Option<v8::Local<'s, v8::Object>> {
    let Some(m) = matrix_arg(scope, v) else {
        throw_type_error(scope, &format!("Failed to execute 'createSVGTransformFromMatrix' on '{iface}': parameter 1 is not of type 'SVGMatrix'."));
        return None;
    };
    create_from(scope, m)
}

fn create_from<'s>(scope: &mut v8::PinScope<'s, '_>, m: [f64; 6]) -> Option<v8::Local<'s, v8::Object>> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let id = new_item(&mut op_state.borrow_mut(), Tx::matrix(m), None);
    tear_off(scope, id, None, false)
}

/// A transform tear-off's `type` / `angle` / `matrix`.
pub(super) fn get(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, id: u32, name: &str, rv: &mut v8::ReturnValue) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let tx = item(&mut op_state.borrow_mut(), id);
    match name {
        "type" => rv.set_double(tx.kind as f64),
        "angle" => rv.set_double(tx.angle as f64),
        // `[SameObject]`: one SVGMatrix per tear-off, reading and writing through to the transform
        "matrix" => {
            if let Some(c) = slot_get(scope, this, "korvus_svg_matrix") {
                rv.set(c);
                return;
            }
            let Some(m) = mint(scope, "SVGMatrix", None, None) else { return };
            slot_set(scope, m, SLOT_TX_OF, this.into());
            slot_set(scope, this, "korvus_svg_matrix", m.into());
            rv.set(m.into());
        }
        _ => {}
    }
}

/// The transform a `matrix` tear-off belongs to: (the transform tear-off, its item).
fn matrix_owner<'s>(scope: &mut v8::PinScope<'s, '_>, m: v8::Local<v8::Object>) -> Option<(v8::Local<'s, v8::Object>, u32)> {
    let t = slot_get(scope, m, SLOT_TX_OF).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let id = tx_of(scope, t)?;
    Some((t, id))
}

/// A transform's `matrix` read: its `[a b c d e f]` now. `None` for any other SVGMatrix.
pub(super) fn matrix_values(scope: &mut v8::PinScope, m: v8::Local<v8::Object>) -> Option<[f64; 6]> {
    let (_, id) = matrix_owner(scope, m)?;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let tx = item(&mut op_state.borrow_mut(), id);
    Some(tx.m)
}

/// A transform's `matrix.a = n` (`SVGMatrixTearOff`): the transform becomes that `matrix`, committed to its element.
/// `false` when `m` is not a transform's matrix. A read-only transform's throws.
pub(super) fn matrix_write(scope: &mut v8::PinScope, m: v8::Local<v8::Object>, name: &str, n: f64) -> bool {
    let Some((t, id)) = matrix_owner(scope, m) else { return false };
    if is_read_only(scope, t) {
        throw_dom(scope, &format!("Failed to set the '{name}' property on 'SVGMatrix': This object is read-only."), "NoModificationAllowedError");
        return true;
    }
    let i = ["a", "b", "c", "d", "e", "f"].iter().position(|k| *k == name).unwrap_or(0);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    {
        let mut st = op_state.borrow_mut();
        let mut tx = item(&mut st, id);
        tx.m[i] = n;
        set_item(&mut st, id, Tx::matrix(tx.m));
    }
    if let Some(el) = num_slot(scope, t, SLOT_TX_EL) {
        commit(scope, el);
    }
    true
}

/// WebIDL `float` (restricted): ToNumber, finite, then the nearest float -- Blink's message when it is not.
fn to_float(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, method: &str, iface: &str) -> Option<f32> {
    let n = v.number_value(scope)?;
    let f = n as f32;
    if !n.is_finite() || !f.is_finite() {
        throw_type_error(scope, &format!("Failed to execute '{method}' on '{iface}': The provided float value is non-finite."));
        return None;
    }
    Some(f)
}

/// An SVGMatrix argument's `[a b c d e f]` -- a detached one or a transform's.
fn matrix_arg(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<[f64; 6]> {
    if !crate::binding::protos::implements(scope, v, "SVGMatrix") {
        return None;
    }
    let o = v8::Local::<v8::Object>::try_from(v).ok()?;
    let a = super::detached_nums(scope, o, &["a", "b", "c", "d", "e", "f"], 0.0)?;
    Some([a[0], a[1], a[2], a[3], a[4], a[5]])
}

/// `setMatrix` / `setTranslate` / `setScale` / `setRotate` / `setSkewX` / `setSkewY` on a transform tear-off: the
/// arguments converted first (each `float`, `setMatrix`'s an SVGMatrix), then Blink's read-only check, then the
/// change, committed to the tear-off's element.
pub(super) fn method(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, this: v8::Local<v8::Object>, id: u32, method: &str) {
    let tx = if method == "setMatrix" {
        let Some(m) = matrix_arg(scope, args.get(0)) else {
            throw_type_error(scope, "Failed to execute 'setMatrix' on 'SVGTransform': parameter 1 is not of type 'SVGMatrix'.");
            return;
        };
        Tx::matrix(m)
    } else {
        let n = match method {
            "setRotate" => 3,
            "setTranslate" | "setScale" => 2,
            _ => 1,
        };
        let mut a = [0.0f32; 3];
        for (i, slot) in a.iter_mut().enumerate().take(n) {
            let Some(f) = to_float(scope, args.get(i as i32), method, "SVGTransform") else { return };
            *slot = f;
        }
        match method {
            "setTranslate" => Tx::translation(a[0], a[1]),
            "setScale" => Tx::scaling(a[0], a[1]),
            "setRotate" => Tx::rotation(a[0], a[1], a[2]),
            "setSkewX" => Tx::skew(T_SKEWX, a[0]),
            _ => Tx::skew(T_SKEWY, a[0]),
        }
    };
    if is_read_only(scope, this) {
        throw_dom(scope, &format!("Failed to execute '{method}' on 'SVGTransform': This object is read-only."), "NoModificationAllowedError");
        return;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    set_item(&mut op_state.borrow_mut(), id, tx);
    if let Some(el) = num_slot(scope, this, SLOT_TX_EL) {
        commit(scope, el);
    }
}

// ── the list ────────────────────────────────────────────────────────────────────────────────────

/// How many items element `el`'s list holds.
pub(super) fn list_len(scope: &mut v8::PinScope, el: u32) -> usize {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let n = list_ids(&mut op_state.borrow_mut(), el).len();
    n
}

/// Item `i` of element `el`'s list: a new tear-off of it (`getItem(0) !== getItem(0)`, Chrome 150).
pub(super) fn list_item<'s>(scope: &mut v8::PinScope<'s, '_>, el: u32, i: usize, read_only: bool) -> Option<v8::Local<'s, v8::Object>> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let id = *list_ids(&mut op_state.borrow_mut(), el).get(i)?;
    tear_off(scope, id, Some(el), read_only)
}

/// Blink's `GetValueForInsertionFromTearOff`: the item a list takes for tear-off `t` -- a COPY when `t` is
/// read-only, already in a list, or bound to an element; else `t`'s own item, `t` now bound to `el`.
fn insertion_item(scope: &mut v8::PinScope, t: v8::Local<v8::Object>, el: u32) -> u32 {
    let id = tx_of(scope, t).unwrap_or(0);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let owned = with_store(&mut op_state.borrow_mut(), |s| s.owner.get(id as usize).copied().flatten().is_some());
    if is_read_only(scope, t) || owned || num_slot(scope, t, SLOT_TX_EL).is_some() {
        let mut st = op_state.borrow_mut();
        let tx = item(&mut st, id);
        return new_item(&mut st, tx, None);
    }
    set_num_slot(scope, t, SLOT_TX_EL, el);
    id
}

fn bound_msg(method: &str, i: u32, n: usize) -> String {
    let eq = if i as usize == n { "or equal to " } else { "" };
    format!("Failed to execute '{method}' on 'SVGTransformList': The index provided ({i}) is greater than {eq}the maximum bound ({n}).")
}

/// The SVGTransformList methods on element `el`'s list (`read_only`: an `animVal`). Blink's order: the arguments
/// converted (an item must be an SVGTransform, an index is ToUint32), then the read-only check, then the range.
pub(super) fn list_method(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, el: u32, read_only: bool, method: &str, rv: &mut v8::ReturnValue) {
    let takes_item = matches!(method, "appendItem" | "insertItemBefore" | "replaceItem" | "initialize");
    let item_arg = if takes_item {
        let v = args.get(0);
        if !crate::binding::protos::implements(scope, v, "SVGTransform") {
            throw_type_error(scope, &format!("Failed to execute '{method}' on 'SVGTransformList': parameter 1 is not of type 'SVGTransform'."));
            return;
        }
        match v8::Local::<v8::Object>::try_from(v) {
            Ok(o) if tx_of(scope, o).is_some() => Some(o),
            _ => {
                throw_type_error(scope, &format!("Failed to execute '{method}' on 'SVGTransformList': parameter 1 is not of type 'SVGTransform'."));
                return;
            }
        }
    } else {
        None
    };
    let index = match method {
        "removeItem" => args.get(0).uint32_value(scope),
        "insertItemBefore" | "replaceItem" => args.get(1).uint32_value(scope),
        _ => Some(0),
    };
    let Some(index) = index else { return };
    if method == "createSVGTransformFromMatrix" {
        if let Some(o) = from_matrix(scope, args.get(0), "SVGTransformList") {
            rv.set(o.into());
        }
        return;
    }
    if read_only {
        throw_dom(scope, &format!("Failed to execute '{method}' on 'SVGTransformList': This object is read-only."), "NoModificationAllowedError");
        return;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut ids = list_ids(&mut op_state.borrow_mut(), el);
    let n = ids.len();
    if matches!(method, "removeItem" | "replaceItem") && index as usize >= n {
        throw_dom(scope, &bound_msg(method, index, n), "IndexSizeError");
        return;
    }
    let ret: Option<(u32, Option<u32>)> = match method {
        "clear" => {
            ids.clear();
            None
        }
        "removeItem" => Some((ids.remove(index as usize), None)),
        "consolidate" => {
            if ids.is_empty() {
                rv.set_null();
                return;
            }
            // `Concatenate`: the identity times each item's matrix in turn; NOT committed (Chrome 150 leaves the
            // attribute as it was while the list holds the one `matrix`)
            let mut st = op_state.borrow_mut();
            let mut m = IDENTITY;
            for &i in &ids {
                m = multiply(m, item(&mut st, i).m);
            }
            let id = new_item(&mut st, Tx::matrix(m), Some(el));
            set_list(&mut st, el, vec![id]);
            drop(st);
            if let Some(o) = tear_off(scope, id, Some(el), false) {
                rv.set(o.into());
            }
            return;
        }
        _ => {
            let Some(t) = item_arg else { return };
            let value = insertion_item(scope, t, el);
            match method {
                "appendItem" => ids.push(value),
                "insertItemBefore" => ids.insert((index as usize).min(n), value),
                "replaceItem" => ids[index as usize] = value,
                _ => ids = vec![value],
            }
            Some((value, Some(el)))
        }
    };
    set_list(&mut op_state.borrow_mut(), el, ids);
    commit(scope, el);
    if let Some((id, bound)) = ret {
        if let Some(o) = tear_off(scope, id, bound, false) {
            rv.set(o.into());
        }
    }
}

/// `list[i] = transform` (Blink's `AnonymousIndexedSetter`): `replaceItem(transform, i)`.
pub(super) fn list_set_index(scope: &mut v8::PinScope, el: u32, read_only: bool, index: u32, v: v8::Local<v8::Value>) {
    let t = match v8::Local::<v8::Object>::try_from(v) {
        Ok(o) if crate::binding::protos::implements(scope, v, "SVGTransform") && tx_of(scope, o).is_some() => o,
        _ => {
            throw_type_error(scope, &format!("Failed to set an indexed property [{index}] on 'SVGTransformList': The provided value is not of type 'SVGTransform'."));
            return;
        }
    };
    if read_only {
        throw_dom(scope, &format!("Failed to set an indexed property [{index}] on 'SVGTransformList': This object is read-only."), "NoModificationAllowedError");
        return;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut ids = list_ids(&mut op_state.borrow_mut(), el);
    if index as usize >= ids.len() {
        let n = ids.len();
        let eq = if index as usize == n { "or equal to " } else { "" };
        throw_dom(scope, &format!("Failed to set an indexed property [{index}] on 'SVGTransformList': The index provided ({index}) is greater than {eq}the maximum bound ({n})."), "IndexSizeError");
        return;
    }
    let value = insertion_item(scope, t, el);
    ids[index as usize] = value;
    set_list(&mut op_state.borrow_mut(), el, ids);
    commit(scope, el);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_print_as_blink_appends_them() {
        for (d, want) in [
            (0.1f32 as f64, "0.1"),
            (1e-7f32 as f64, "1.00000e-7"),
            (1234567.0, "1.23457e+6"),
            (0.000012345f32 as f64, "0.000012345"),
            (-0.5, "-0.5"),
            (-1e-8f32 as f64, "-1.00000e-8"),
            (7.25, "7.25"),
            (10.0, "10"),
            (123456.0, "123456"),
            (1234565.0, "1.23457e+6"),
            (0.0, "0"),
        ] {
            assert_eq!(blink_number(d), want, "{d}");
        }
    }

    #[test]
    fn a_rotation_serializes_its_centre_as_blink_decomposes_it() {
        assert_eq!(Tx::rotation(30.0, 0.0, 5.0).value_as_string(), "rotate(30 3.64262e-8 5)");
        assert_eq!(Tx::rotation(45.0, 10.0, 20.0).value_as_string(), "rotate(45 10 20)");
        assert_eq!(Tx::rotation(90.0, 3.0, 4.0).value_as_string(), "rotate(90 3 4)");
        assert_eq!(Tx::rotation(30.0, 0.0, 0.0).value_as_string(), "rotate(30)");
    }

    #[test]
    fn the_list_grammar_is_blinks() {
        let s = |t: &str| parse_list(t).map(|l| l.iter().map(|x| x.value_as_string()).collect::<Vec<_>>().join(" "));
        assert_eq!(s("translate(1,2),scale(3)").as_deref(), Some("translate(1 2) scale(3 3)"));
        assert_eq!(s("  scale( 2 , 3 )  translate(1e1 .5)  ").as_deref(), Some("scale(2 3) translate(10 0.5)"));
        assert_eq!(s("scale(2) foo(3)"), None);
        assert_eq!(s("scale(2),"), None);
        assert_eq!(s("translate(0.3 1.23456789)").as_deref(), Some("translate(0.3 1.23457)"));
        assert_eq!(parse_list("translate(0.3 1.23456789)").unwrap()[0].m[5], 1.2345678806304932);
    }
}
