//! `DOMMatrixReadOnly` / `DOMMatrix` — the native 4x4 matrix family + CSS-transform parser.

use deno_core::{v8, CppgcBase, CppgcInherits};
use deno_error::JsErrorBox;
use macros::webapi;

use super::syntax_err;
use super::dom_point::{DOMPoint, DOMPointReadOnly, PointInit};

// ---- DOMMatrixReadOnly / DOMMatrix — native cppgc with the full 4x4 algebra and
// CSS-transform parser ported from the JS glue. Storage is column-major [f64;16] (m[c*4+r]) + is2D.

const IDENTITY: [f64; 16] = [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.];

fn mat_mul(a: &[f64; 16], b: &[f64; 16]) -> [f64; 16] {
    let mut r = [0.0; 16];
    for c in 0..4 {
        for rr in 0..4 {
            let mut s = 0.0;
            for k in 0..4 { s += a[k * 4 + rr] * b[c * 4 + k]; }
            r[c * 4 + rr] = s;
        }
    }
    r
}

fn mat_invert(m: &[f64; 16]) -> Option<[f64; 16]> {
    let mut inv = [0.0; 16];
    inv[0] = m[5]*m[10]*m[15]-m[5]*m[11]*m[14]-m[9]*m[6]*m[15]+m[9]*m[7]*m[14]+m[13]*m[6]*m[11]-m[13]*m[7]*m[10];
    inv[4] = -m[4]*m[10]*m[15]+m[4]*m[11]*m[14]+m[8]*m[6]*m[15]-m[8]*m[7]*m[14]-m[12]*m[6]*m[11]+m[12]*m[7]*m[10];
    inv[8] = m[4]*m[9]*m[15]-m[4]*m[11]*m[13]-m[8]*m[5]*m[15]+m[8]*m[7]*m[13]+m[12]*m[5]*m[11]-m[12]*m[7]*m[9];
    inv[12] = -m[4]*m[9]*m[14]+m[4]*m[10]*m[13]+m[8]*m[5]*m[14]-m[8]*m[6]*m[13]-m[12]*m[5]*m[10]+m[12]*m[6]*m[9];
    inv[1] = -m[1]*m[10]*m[15]+m[1]*m[11]*m[14]+m[9]*m[2]*m[15]-m[9]*m[3]*m[14]-m[13]*m[2]*m[11]+m[13]*m[3]*m[10];
    inv[5] = m[0]*m[10]*m[15]-m[0]*m[11]*m[14]-m[8]*m[2]*m[15]+m[8]*m[3]*m[14]+m[12]*m[2]*m[11]-m[12]*m[3]*m[10];
    inv[9] = -m[0]*m[9]*m[15]+m[0]*m[11]*m[13]+m[8]*m[1]*m[15]-m[8]*m[3]*m[13]-m[12]*m[1]*m[11]+m[12]*m[3]*m[9];
    inv[13] = m[0]*m[9]*m[14]-m[0]*m[10]*m[13]-m[8]*m[1]*m[14]+m[8]*m[2]*m[13]+m[12]*m[1]*m[10]-m[12]*m[2]*m[9];
    inv[2] = m[1]*m[6]*m[15]-m[1]*m[7]*m[14]-m[5]*m[2]*m[15]+m[5]*m[3]*m[14]+m[13]*m[2]*m[7]-m[13]*m[3]*m[6];
    inv[6] = -m[0]*m[6]*m[15]+m[0]*m[7]*m[14]+m[4]*m[2]*m[15]-m[4]*m[3]*m[14]-m[12]*m[2]*m[7]+m[12]*m[3]*m[6];
    inv[10] = m[0]*m[5]*m[15]-m[0]*m[7]*m[13]-m[4]*m[1]*m[15]+m[4]*m[3]*m[13]+m[12]*m[1]*m[7]-m[12]*m[3]*m[5];
    inv[14] = -m[0]*m[5]*m[14]+m[0]*m[6]*m[13]+m[4]*m[1]*m[14]-m[4]*m[2]*m[13]-m[12]*m[1]*m[6]+m[12]*m[2]*m[5];
    inv[3] = -m[1]*m[6]*m[11]+m[1]*m[7]*m[10]+m[5]*m[2]*m[11]-m[5]*m[3]*m[10]-m[9]*m[2]*m[7]+m[9]*m[3]*m[6];
    inv[7] = m[0]*m[6]*m[11]-m[0]*m[7]*m[10]-m[4]*m[2]*m[11]+m[4]*m[3]*m[10]+m[8]*m[2]*m[7]-m[8]*m[3]*m[6];
    inv[11] = -m[0]*m[5]*m[11]+m[0]*m[7]*m[9]+m[4]*m[1]*m[11]-m[4]*m[3]*m[9]-m[8]*m[1]*m[7]+m[8]*m[3]*m[5];
    inv[15] = m[0]*m[5]*m[10]-m[0]*m[6]*m[9]-m[4]*m[1]*m[10]+m[4]*m[2]*m[9]+m[8]*m[1]*m[6]-m[8]*m[2]*m[5];
    let det = m[0]*inv[0]+m[1]*inv[4]+m[2]*inv[8]+m[3]*inv[12];
    if det == 0.0 { return None; }
    let det = 1.0 / det;
    for x in inv.iter_mut() { *x *= det; }
    Some(inv)
}

// ---- CSS <transform-list> parser --------------------------------------------------------------
//
// `new DOMMatrix(string)` and `setMatrixValue(string)` parse a CSS <transform-list> and must REJECT
// anything that is not one, with a SyntaxError DOMException. The parser this replaces accepted far
// too much: it split arguments on every comma at any depth, ignored units entirely (`translateX(5)`
// and `translateX(5em)` both "worked"), never noticed a 3D function, and could not see a space
// between a function name and its parenthesis. That was 106 of the 233 css/geometry WPT subtests.
//
// This one tokenizes properly: comments are whitespace, a function token is `ident(` with NOTHING
// between, arguments split on TOP-LEVEL commas only, and a length must carry an ABSOLUTE unit --
// the string is parsed with no element and no viewport, so em/rem/%/vw/cq* are errors rather than
// zeroes. calc() is evaluated with unit arithmetic, so `calc(2 * 2.5px)` is 5px while
// `calc(10px * sign(1em - 10px))` is the error it has to be.

use std::f64::consts::PI;

/// What a parsed component value turned out to be. A `<number>`, an absolute `<length>` already
/// converted to px, or an `<angle>` already converted to degrees.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Unit { Num, Px, Deg }

#[derive(Clone, Copy, Debug)]
struct Dim { v: f64, u: Unit }

/// A byte cursor over an ASCII CSS fragment.
struct P<'a> { b: &'a [u8], i: usize }

impl<'a> P<'a> {
    fn new(s: &'a str) -> Self { P { b: s.as_bytes(), i: 0 } }
    fn peek(&self) -> Option<u8> { self.b.get(self.i).copied() }
    fn ws(&mut self) { while matches!(self.peek(), Some(c) if c.is_ascii_whitespace()) { self.i += 1; } }
    fn eat(&mut self, c: u8) -> bool { if self.peek() == Some(c) { self.i += 1; true } else { false } }
    fn done(&self) -> bool { self.i >= self.b.len() }
    fn slice(&self, from: usize) -> &'a str { std::str::from_utf8(&self.b[from..self.i]).unwrap_or("\u{0}") }
}

/// `<number>` / `<dimension>` / `<percentage>` token. Percentages and every relative unit resolve
/// against a box this parser does not have, so they are rejected rather than guessed at.
fn dim_token(p: &mut P) -> Result<Dim, JsErrorBox> {
    let start = p.i;
    if matches!(p.peek(), Some(b'+') | Some(b'-')) { p.i += 1; }
    let mut digits = false;
    while matches!(p.peek(), Some(c) if c.is_ascii_digit()) { p.i += 1; digits = true; }
    if p.peek() == Some(b'.') {
        p.i += 1;
        while matches!(p.peek(), Some(c) if c.is_ascii_digit()) { p.i += 1; digits = true; }
    }
    if !digits { return Err(syntax_err("expected a number")); }
    if matches!(p.peek(), Some(b'e') | Some(b'E')) {
        let save = p.i;
        p.i += 1;
        if matches!(p.peek(), Some(b'+') | Some(b'-')) { p.i += 1; }
        if matches!(p.peek(), Some(c) if c.is_ascii_digit()) {
            while matches!(p.peek(), Some(c) if c.is_ascii_digit()) { p.i += 1; }
        } else {
            p.i = save; // `2ex` is a dimension, not a broken exponent
        }
    }
    let n: f64 = p.slice(start).parse().map_err(|_| syntax_err("expected a number"))?;
    let us = p.i;
    if p.eat(b'%') { return Err(syntax_err("a percentage needs a layout box")); }
    while matches!(p.peek(), Some(c) if c.is_ascii_alphabetic()) { p.i += 1; }
    let unit = p.slice(us).to_ascii_lowercase();
    Ok(match unit.as_str() {
        "" => Dim { v: n, u: Unit::Num },
        // The absolute lengths (css-values-3 5.2), all anchored to 1in = 96px.
        "px" => Dim { v: n, u: Unit::Px },
        "in" => Dim { v: n * 96.0, u: Unit::Px },
        "cm" => Dim { v: n * 96.0 / 2.54, u: Unit::Px },
        "mm" => Dim { v: n * 96.0 / 25.4, u: Unit::Px },
        "q" => Dim { v: n * 96.0 / 101.6, u: Unit::Px },
        "pt" => Dim { v: n * 96.0 / 72.0, u: Unit::Px },
        "pc" => Dim { v: n * 16.0, u: Unit::Px },
        "deg" => Dim { v: n, u: Unit::Deg },
        "grad" => Dim { v: n * 0.9, u: Unit::Deg },
        "rad" => Dim { v: n * 180.0 / PI, u: Unit::Deg },
        "turn" => Dim { v: n * 360.0, u: Unit::Deg },
        other => return Err(syntax_err(&format!("unit `{other}` needs a layout context"))),
    })
}

fn calc_factor(p: &mut P) -> Result<Dim, JsErrorBox> {
    p.ws();
    if p.eat(b'(') {
        let v = calc_expr(p)?;
        p.ws();
        if !p.eat(b')') { return Err(syntax_err("unbalanced parenthesis")); }
        return Ok(v);
    }
    if matches!(p.peek(), Some(c) if c.is_ascii_alphabetic()) {
        let start = p.i;
        while matches!(p.peek(), Some(c) if c.is_ascii_alphanumeric() || c == b'-') { p.i += 1; }
        let name = p.slice(start).to_ascii_lowercase();
        // calc() is the only math function that resolves without style or layout. sign(), min(),
        // clamp(), sibling-index(), var(), attr() ... all need context this parser has none of, and
        // "guess a number" would be worse than the SyntaxError the spec asks for.
        if name != "calc" || !p.eat(b'(') { return Err(syntax_err(&format!("`{name}` is not a supported value"))); }
        let v = calc_expr(p)?;
        p.ws();
        if !p.eat(b')') { return Err(syntax_err("unbalanced parenthesis")); }
        return Ok(v);
    }
    dim_token(p)
}

fn calc_term(p: &mut P) -> Result<Dim, JsErrorBox> {
    let mut left = calc_factor(p)?;
    loop {
        p.ws();
        match p.peek() {
            Some(b'*') => {
                p.i += 1;
                let r = calc_factor(p)?;
                left = if left.u == Unit::Num { Dim { v: left.v * r.v, u: r.u } }
                    else if r.u == Unit::Num { Dim { v: left.v * r.v, u: left.u } }
                    else { return Err(syntax_err("calc(): cannot multiply two dimensions")); };
            }
            Some(b'/') => {
                p.i += 1;
                let r = calc_factor(p)?;
                if r.u != Unit::Num { return Err(syntax_err("calc(): divisor must be a number")); }
                left = Dim { v: left.v / r.v, u: left.u };
            }
            _ => return Ok(left),
        }
    }
}

fn calc_expr(p: &mut P) -> Result<Dim, JsErrorBox> {
    let mut left = calc_term(p)?;
    loop {
        p.ws();
        let op = match p.peek() { Some(c @ (b'+' | b'-')) => c, _ => return Ok(left) };
        p.i += 1;
        let r = calc_term(p)?;
        if left.u != r.u { return Err(syntax_err("calc(): mismatched units")); }
        left = Dim { v: if op == b'+' { left.v + r.v } else { left.v - r.v }, u: left.u };
    }
}

/// One whole transform-function argument: a single component value (a token or a calc()), nothing
/// after it. `scale(2 2)` is one argument of two tokens, which is exactly why it must fail here.
fn parse_value(a: &str) -> Result<Dim, JsErrorBox> {
    let mut p = P::new(a);
    let d = calc_factor(&mut p)?;
    p.ws();
    if !p.done() { return Err(syntax_err("unexpected trailing tokens")); }
    Ok(d)
}

fn arg_num(a: &str) -> Result<f64, JsErrorBox> {
    match parse_value(a)? { Dim { v, u: Unit::Num } => Ok(v), _ => Err(syntax_err("expected a number")) }
}
/// A `<length>`: an absolute unit, or a unitless zero (css-values lets the unit go on a zero length).
fn arg_px(a: &str) -> Result<f64, JsErrorBox> {
    match parse_value(a)? {
        Dim { v, u: Unit::Px } => Ok(v),
        Dim { v, u: Unit::Num } if v == 0.0 => Ok(0.0),
        _ => Err(syntax_err("expected a length")),
    }
}
fn arg_deg(a: &str) -> Result<f64, JsErrorBox> {
    match parse_value(a)? {
        Dim { v, u: Unit::Deg } => Ok(v),
        Dim { v, u: Unit::Num } if v == 0.0 => Ok(0.0),
        _ => Err(syntax_err("expected an angle")),
    }
}

/// Blink's `gfx::SinCosDegrees` (`ui/gfx/geometry/sin_cos_degrees.h`): a multiple of 45 degrees is
/// EXACT (`rotate(90deg)` is `matrix(0, 1, -1, 0, 0, 0)`, not `6.123233995736766e-17`), anything else is
/// range-reduced to [0, 45] before `sin` / `cos` -- so `sin(30deg)` is `0.49999999999999994` and
/// `sin(45deg)` `0.7071067811865476`, Chrome 150's answers (measured).
pub(crate) fn sin_cos_degrees(degrees: f64) -> (f64, f64) {
    if degrees > -90000000.0 && degrees < 90000000.0 {
        let n45 = degrees / 45.0;
        let mut octant = n45 as i64;
        if octant as f64 == n45 {
            const H: f64 = std::f64::consts::SQRT_2 / 2.0;
            const T: [(f64, f64); 8] = [(0., 1.), (H, H), (1., 0.), (H, -H), (0., -1.), (-H, -H), (-1., 0.), (-H, H)];
            return T[(octant & 7) as usize];
        }
        if degrees < 0.0 {
            octant -= 1;
        }
        let mut d = degrees - octant as f64 * 45.0;
        if octant & 1 != 0 {
            d = 45.0 - d;
        }
        let rad = d * PI / 180.0;
        let (mut s, mut c) = (rad.sin(), rad.cos());
        if (octant + 1) & 2 != 0 {
            std::mem::swap(&mut s, &mut c);
        }
        if octant & 4 != 0 {
            s = -s;
        }
        if (octant + 2) & 4 != 0 {
            c = -c;
        }
        return (s, c);
    }
    let rad = (degrees % 360.0) * PI / 180.0;
    (rad.sin(), rad.cos())
}

/// The CSS `rotate3d(x, y, z, angle)` matrix, shared by every rotation entry point, as Blink builds it
/// (`gfx::Transform::RotateAbout` -> `Matrix44::RotateUnitSinCos`): the axis scaled by `1/sqrt` of its
/// squared length, a unit major axis taking the exact single-axis rotation, and the general case
/// Blink's own products. A vector that cannot be normalized leaves the matrix alone, and so does an
/// angle whose sine is 0 and cosine 1.
fn axis_rot(x: f64, y: f64, z: f64, deg: f64) -> [f64; 16] {
    let (s, c) = sin_cos_degrees(deg);
    if s == 0.0 && c == 1.0 {
        return IDENTITY;
    }
    let sq = x * x + y * y + z * z;
    if sq == 0.0 || !sq.is_finite() {
        return IDENTITY;
    }
    let (mut x, mut y, mut z) = (x, y, z);
    if sq != 1.0 {
        let scale = 1.0 / sq.sqrt();
        x *= scale;
        y *= scale;
        z *= scale;
    }
    if z == 1.0 {
        return [c, s, 0., 0., -s, c, 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.];
    }
    if y == 1.0 {
        return [c, 0., -s, 0., 0., 1., 0., 0., s, 0., c, 0., 0., 0., 0., 1.];
    }
    if x == 1.0 {
        return [1., 0., 0., 0., 0., c, s, 0., 0., -s, c, 0., 0., 0., 0., 1.];
    }
    let cc = 1.0 - c;
    let (xs, ys, zs) = (x * s, y * s, z * s);
    let (xc, yc, zc) = (x * cc, y * cc, z * cc);
    let (xyc, yzc, zxc) = (x * yc, y * zc, z * xc);
    [
        x * xc + c, xyc + zs, zxc - ys, 0.,
        xyc - zs, y * yc + c, yzc + xs, 0.,
        zxc + ys, yzc - xs, z * zc + c, 0.,
        0., 0., 0., 1.,
    ]
}

/// ECMAScript `Number::toString(10)`, which the `DOMMatrix` stringifier prints each entry with
/// (`1e-17`, `1e+21`, `6.123233995736766e-17` -- Chrome 150, measured).
pub(crate) fn js_number(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x == 0.0 {
        return "0".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    let sign = if x < 0.0 { "-" } else { "" };
    // Rust's `{:e}` is the shortest round-trip digit string, as ECMAScript requires.
    let e = format!("{:e}", x.abs());
    let (mant, exp) = e.split_once('e').unwrap_or((&e, "0"));
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i64;
    let n = exp.parse::<i64>().unwrap_or(0) + 1;
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let e = n - 1;
        let es = if e < 0 { format!("-{}", -e) } else { format!("+{e}") };
        if k == 1 { format!("{digits}e{es}") } else { format!("{}.{}e{es}", &digits[..1], &digits[1..]) }
    };
    format!("{sign}{body}")
}

/// One transform function -> (its matrix, whether it is a 3D transform function). The 3D set is
/// css-transforms-2 3; a list containing any of them parses to `is2D === false`, whatever the
/// numbers happen to look like.
fn build_transform(name: &str, args: &[&str]) -> Result<([f64; 16], bool), JsErrorBox> {
    let bad = || syntax_err(&format!("`{name}()` got the wrong arguments"));
    let mut t = IDENTITY;
    let n = args.len();
    let mut is3d = false;
    match name {
        "matrix" => {
            if n != 6 { return Err(bad()); }
            let a = args.iter().map(|x| arg_num(x)).collect::<Result<Vec<f64>, _>>()?;
            t = [a[0], a[1], 0., 0., a[2], a[3], 0., 0., 0., 0., 1., 0., a[4], a[5], 0., 1.];
        }
        "matrix3d" => {
            if n != 16 { return Err(bad()); }
            for (i, x) in args.iter().enumerate() { t[i] = arg_num(x)?; }
            is3d = true;
        }
        "translate" => {
            if !(1..=2).contains(&n) { return Err(bad()); }
            t[12] = arg_px(args[0])?;
            t[13] = if n == 2 { arg_px(args[1])? } else { 0. };
        }
        "translatex" => { if n != 1 { return Err(bad()); } t[12] = arg_px(args[0])?; }
        "translatey" => { if n != 1 { return Err(bad()); } t[13] = arg_px(args[0])?; }
        "translatez" => { if n != 1 { return Err(bad()); } t[14] = arg_px(args[0])?; is3d = true; }
        "translate3d" => {
            if n != 3 { return Err(bad()); }
            t[12] = arg_px(args[0])?; t[13] = arg_px(args[1])?; t[14] = arg_px(args[2])?;
            is3d = true;
        }
        "scale" => {
            if !(1..=2).contains(&n) { return Err(bad()); }
            let sx = arg_num(args[0])?;
            t[0] = sx;
            t[5] = if n == 2 { arg_num(args[1])? } else { sx };
        }
        "scalex" => { if n != 1 { return Err(bad()); } t[0] = arg_num(args[0])?; }
        "scaley" => { if n != 1 { return Err(bad()); } t[5] = arg_num(args[0])?; }
        "scalez" => { if n != 1 { return Err(bad()); } t[10] = arg_num(args[0])?; is3d = true; }
        "scale3d" => {
            if n != 3 { return Err(bad()); }
            t[0] = arg_num(args[0])?; t[5] = arg_num(args[1])?; t[10] = arg_num(args[2])?;
            is3d = true;
        }
        "rotate" => { if n != 1 { return Err(bad()); } t = axis_rot(0., 0., 1., arg_deg(args[0])?); }
        "rotatex" => { if n != 1 { return Err(bad()); } t = axis_rot(1., 0., 0., arg_deg(args[0])?); is3d = true; }
        "rotatey" => { if n != 1 { return Err(bad()); } t = axis_rot(0., 1., 0., arg_deg(args[0])?); is3d = true; }
        "rotatez" => { if n != 1 { return Err(bad()); } t = axis_rot(0., 0., 1., arg_deg(args[0])?); is3d = true; }
        "rotate3d" => {
            if n != 4 { return Err(bad()); }
            t = axis_rot(arg_num(args[0])?, arg_num(args[1])?, arg_num(args[2])?, arg_deg(args[3])?);
            is3d = true;
        }
        "skew" => {
            if !(1..=2).contains(&n) { return Err(bad()); }
            t[4] = (arg_deg(args[0])? * PI / 180.0).tan();
            if n == 2 { t[1] = (arg_deg(args[1])? * PI / 180.0).tan(); }
        }
        "skewx" => { if n != 1 { return Err(bad()); } t[4] = (arg_deg(args[0])? * PI / 180.0).tan(); }
        "skewy" => { if n != 1 { return Err(bad()); } t[1] = (arg_deg(args[0])? * PI / 180.0).tan(); }
        "perspective" => {
            if n != 1 { return Err(bad()); }
            // `perspective( <length [0,\u{221e}]> | none )`. The range is NON-NEGATIVE, so zero is a
            // legal argument and only a negative length is a parse error. A zero depth makes the
            // projection degenerate rather than invalid, and Blink renders it as the identity, which
            // is what leaving t[11] alone does here. Rejecting it -- as an earlier draft did -- was
            // inconsistent with this same parser already accepting the `none` form from the same
            // (newer) spec revision.
            if !args[0].trim().eq_ignore_ascii_case("none") {
                let d = arg_px(args[0])?;
                if d < 0.0 { return Err(bad()); }
                if d > 0.0 { t[11] = -1.0 / d; }
            }
            is3d = true;
        }
        other => return Err(syntax_err(&format!("unknown transform function `{other}`"))),
    }
    Ok((t, is3d))
}

/// CSS comments are whitespace: `none/**/` is `none`, and `/**/` on its own is an empty (invalid)
/// transform list rather than the identity.
fn strip_comments(s: &str) -> Result<String, JsErrorBox> {
    if !s.contains("/*") { return Ok(s.to_string()); }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        match rest.find("/*") {
            None => { out.push_str(rest); return Ok(out); }
            Some(k) => {
                out.push_str(&rest[..k]);
                out.push(' ');
                let after = &rest[k + 2..];
                match after.find("*/") {
                    Some(j) => rest = &after[j + 2..],
                    None => return Err(syntax_err("unterminated comment")),
                }
            }
        }
    }
}

/// Split a function's argument text on TOP-LEVEL commas. `translateX(calc(2 * 2.5px))` has none,
/// but a nested function that did would be torn in half by a naive split.
fn split_top(s: &str) -> Vec<&str> {
    if s.trim().is_empty() { return Vec::new(); }
    let mut out = Vec::new();
    let (mut depth, mut start) = (0i32, 0usize);
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => { out.push(&s[start..i]); start = i + 1; }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

/// Parse a CSS <transform-list> -> (matrix, is2D). Only the EMPTY string and `none` (ASCII
/// case-insensitive, comments and whitespace allowed around it) are the identity; `" "` and `"/**/"`
/// are empty transform lists, which is a SyntaxError.
pub(crate) fn parse_transform(s: &str) -> Result<([f64; 16], bool), JsErrorBox> {
    if s.is_empty() { return Ok((IDENTITY, true)); }
    let stripped = strip_comments(s)?;
    let t = stripped.trim();
    if t.eq_ignore_ascii_case("none") { return Ok((IDENTITY, true)); }
    if t.is_empty() { return Err(syntax_err("invalid transform")); }
    let mut m = IDENTITY;
    let mut any3d = false;
    let mut p = P::new(t);
    loop {
        p.ws();
        if p.done() { break; }
        let start = p.i;
        while matches!(p.peek(), Some(c) if c.is_ascii_alphanumeric()) { p.i += 1; }
        if p.i == start { return Err(syntax_err("invalid transform")); }
        let name = p.slice(start).to_ascii_lowercase();
        // A CSS function token is `ident(` with nothing in between: `translateX    (5px)` is an
        // ident followed by a block, which is not a transform function.
        if !p.eat(b'(') { return Err(syntax_err("invalid transform")); }
        let open = p.i;
        let mut depth = 1u32;
        while let Some(c) = p.peek() {
            p.i += 1;
            match c {
                b'(' => depth += 1,
                b')' => { depth -= 1; if depth == 0 { break; } }
                _ => {}
            }
        }
        if depth != 0 { return Err(syntax_err("invalid transform")); }
        let inner = std::str::from_utf8(&p.b[open..p.i - 1]).map_err(|_| syntax_err("invalid transform"))?;
        let (f, is3d) = build_transform(&name, &split_top(inner))?;
        m = mat_mul(&m, &f);
        any3d |= is3d;
    }
    Ok((m, !any3d))
}

/// Resolve a constructor argument (string CSS / 6- or 16-array / undefined). No dict arm: WebIDL's
/// `optional (DOMString or sequence<unrestricted double>) init` union has only two, and a plain
/// object (or a live matrix, which is not iterable) goes through `ToString` like everything else
/// that is not a sequence -- so `new DOMMatrix(new DOMMatrix())` parses the receiver's own
/// `toString()` output, and `new DOMMatrix({a:2})` stringifies to `"[object Object]"` and throws a
/// SyntaxError, exactly as Chrome does.
fn matrix_from_array(nums: &[f64]) -> Result<([f64; 16], bool), JsErrorBox> {
    if nums.len() == 6 {
        Ok(([nums[0], nums[1], 0., 0., nums[2], nums[3], 0., 0., 0., 0., 1., 0., nums[4], nums[5], 0., 1.], true))
    } else if nums.len() == 16 {
        let mut m = [0.0; 16]; m.copy_from_slice(nums); Ok((m, false))
    } else {
        Err(JsErrorBox::type_error("DOMMatrix init array must have 6 or 16 elements"))
    }
}

/// Is `v` a sequence (an object/function with a callable `[Symbol.iterator]`)? A primitive
/// (string, number, `null`) is never an Object in the v8 sense, so this is false for them without a
/// separate `null`/primitive check -- the same test `dom/00-bootstrap.js`'s deleted `__matrixCtor`
/// Proxy ran before handing the argument to the native constructor as either an
/// array or a string.
fn is_iterable(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> bool {
    let Ok(obj) = v8::Local::<v8::Object>::try_from(v) else { return false };
    let sym = v8::Symbol::get_iterator(scope);
    obj.get(scope, sym.into()).map(|f| f.is_function()).unwrap_or(false)
}

/// Materialise an iterable's elements, like `Array.from`: a fast path for an actual `Array`, and a
/// real `Array.from` call (so a `Set`, a generator or a typed array all work) otherwise. `None` only
/// when `Array` or `.from` cannot be found on the global (never happens outside a broken bootstrap).
fn iterate_all<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<v8::Value>) -> Option<Vec<v8::Local<'s, v8::Value>>> {
    if let Ok(arr) = v8::Local::<v8::Array>::try_from(v) {
        return Some((0..arr.length()).filter_map(|i| arr.get_index(scope, i)).collect());
    }
    let from = crate::binding::protos::intrinsic_fn(scope, "Array.from")?;
    let recv: v8::Local<v8::Value> = v8::undefined(scope).into();
    let arr = from.call(scope, recv, &[v])?;
    let arr = v8::Local::<v8::Array>::try_from(arr).ok()?;
    Some((0..arr.length()).filter_map(|i| arr.get_index(scope, i)).collect())
}

/// WebIDL union resolution for the DOMMatrix(ReadOnly) constructor argument: `optional (DOMString or
/// sequence<unrestricted double>) init`. Replaces `dom/00-bootstrap.js`'s `__matrixCtor` Proxy,
/// which existed because the global on which a `cppgc` row could verify had to be the
/// Proxy's TARGET, not the Proxy -- moving the resolution in here removes the Proxy and lets the
/// native constructor be that target directly (`install::live::reconcile_dom_matrix_family`).
/// `undefined`/absent is the identity; a Symbol is special-cased so its message matches Chrome's own
/// ("Cannot convert a Symbol value to a ...") rather than a generic op2 rethrow.
fn resolve_ctor_arg(scope: &mut v8::PinScope, init: v8::Local<v8::Value>, iface: &str) -> Result<([f64; 16], bool), JsErrorBox> {
    if init.is_undefined() {
        return Ok((IDENTITY, true));
    }
    if is_iterable(scope, init) {
        let items = iterate_all(scope, init).ok_or_else(|| JsErrorBox::type_error("DOMMatrix: could not read the sequence"))?;
        let mut nums = Vec::with_capacity(items.len());
        for it in items {
            if it.is_symbol() {
                return Err(JsErrorBox::type_error("Cannot convert a Symbol value to a number"));
            }
            let n = it.number_value(scope).ok_or_else(|| JsErrorBox::type_error("DOMMatrix: could not convert a sequence element to a number"))?;
            nums.push(n);
        }
        return matrix_from_array(&nums);
    }
    if init.is_symbol() {
        return Err(JsErrorBox::type_error(format!("Failed to construct '{iface}': Cannot convert a Symbol value to a string")));
    }
    let s = init.to_string(scope).ok_or_else(|| JsErrorBox::type_error("DOMMatrix: could not convert the argument to a string"))?;
    let s = s.to_rust_string_lossy(scope);
    // Blink's sentence for a string the transform grammar refuses (Chrome 150: `new DOMMatrix('5')` is "Failed to
    // construct 'DOMMatrix': Failed to parse '5'."; korvus answered "invalid transform")
    parse_transform(&s).map_err(|_| syntax_err(&format!("Failed to construct '{iface}': Failed to parse '{s}'.")))
}

/// `DOMMatrixInit` "validate and fixup" with `at`, the operation's context, in front of its refusal -- see
/// [`MatrixLike::fixup`].
///
/// Takes an already-deserialized [`MatrixLike`] rather than reading v8 values directly: the binding converted the
/// page's dictionary first (`#[dict("DOMMatrixInit")]`, `ctor_dicts::convert_dict`), each member read once with
/// `v8::Object::get` -- which walks the prototype chain exactly like a page's `m[k]` -- so a LIVE `DOMMatrix`
/// argument (whose a/b/c…/m44 are prototype getters, not own properties) resolves the same way a plain `{a:2}`
/// literal does.
fn validate_and_fixup(ml: &MatrixLike, at: &str) -> Result<([f64; 16], bool), JsErrorBox> {
    ml.fixup().map_err(|why| JsErrorBox::type_error(format!("{at}{why}")))
}

/// A DOMMatrix-like argument for multiply/etc: read via getters (works for a native or JS matrix
/// instance) or plain dict keys.
#[derive(serde::Deserialize, Default)]
pub(crate) struct MatrixLike {
    a: Option<f64>, b: Option<f64>, c: Option<f64>, d: Option<f64>, e: Option<f64>, f: Option<f64>,
    m11: Option<f64>, m12: Option<f64>, m13: Option<f64>, m14: Option<f64>,
    m21: Option<f64>, m22: Option<f64>, m23: Option<f64>, m24: Option<f64>,
    m31: Option<f64>, m32: Option<f64>, m33: Option<f64>, m34: Option<f64>,
    m41: Option<f64>, m42: Option<f64>, m43: Option<f64>, m44: Option<f64>,
    #[serde(rename = "is2D")] is_2d: Option<bool>,
}

impl MatrixLike {
    /// Blink's `DOMMatrixReadOnly::ValidateAndFixup` (Chromium 150): a 2D member and its 3D alias that both are
    /// present must agree (NaN agrees with NaN) -- "Property mismatch on matrix initialization."; each 3D alias
    /// absent takes its 2D member, else its identity value; `is2D: true` with a 3D member off its identity value is
    /// "The is2D member is set to true but the input matrix is a 3d matrix."; an absent `is2D` is whether every 3D
    /// member is at its identity value. The converted dictionary holds `m13`..`m44` at their IDL defaults when the
    /// page gave none. korvus answered its own "a and m11 conflict" sentence, refused NaN against NaN, and let
    /// `is2D: true` over a 3D matrix through.
    pub(crate) fn fixup(&self) -> Result<([f64; 16], bool), &'static str> {
        let pair = |short: Option<f64>, long: Option<f64>, dflt: f64| -> Result<f64, &'static str> {
            if let (Some(a), Some(b)) = (short, long) {
                if a != b && !(a.is_nan() && b.is_nan()) {
                    return Err("Property mismatch on matrix initialization.");
                }
            }
            Ok(long.or(short).unwrap_or(dflt))
        };
        let (m11, m12, m21, m22, m41, m42) =
            (pair(self.a, self.m11, 1.)?, pair(self.b, self.m12, 0.)?, pair(self.c, self.m21, 0.)?, pair(self.d, self.m22, 1.)?, pair(self.e, self.m41, 0.)?, pair(self.f, self.m42, 0.)?);
        let (m13, m14, m23, m24) = (self.m13.unwrap_or(0.), self.m14.unwrap_or(0.), self.m23.unwrap_or(0.), self.m24.unwrap_or(0.));
        let (m31, m32, m33, m34) = (self.m31.unwrap_or(0.), self.m32.unwrap_or(0.), self.m33.unwrap_or(1.), self.m34.unwrap_or(0.));
        let (m43, m44) = (self.m43.unwrap_or(0.), self.m44.unwrap_or(1.));
        // C++'s truthiness: a NaN member counts as set (`NaN != 0`), as `m33 != 1` does for NaN
        let three_d = [m31, m32, m13, m23, m43, m14, m24, m34].iter().any(|v| *v != 0.) || m33 != 1. || m44 != 1.;
        if self.is_2d == Some(true) && three_d {
            return Err("The is2D member is set to true but the input matrix is a 3d matrix.");
        }
        let m = [m11, m12, m13, m14, m21, m22, m23, m24, m31, m32, m33, m34, m41, m42, m43, m44];
        Ok((m, self.is_2d.unwrap_or(!three_d)))
    }
}

/// `toJSON()`'s object, in Blink's builder order (`dom_matrix_read_only.cc:422-450`).
#[derive(serde::Serialize)]
pub(crate) struct MatrixJson {
    a: f64, b: f64, c: f64, d: f64, e: f64, f: f64,
    m11: f64, m12: f64, m13: f64, m14: f64, m21: f64, m22: f64, m23: f64, m24: f64,
    m31: f64, m32: f64, m33: f64, m34: f64, m41: f64, m42: f64, m43: f64, m44: f64,
    #[serde(rename = "is2D")] is_2d: bool,
    #[serde(rename = "isIdentity")] is_identity: bool,
}

#[repr(C)]
#[derive(CppgcBase)]
pub struct DOMMatrixReadOnly {
    m: std::cell::Cell<[f64; 16]>,
    is2d: std::cell::Cell<bool>,
}

impl DOMMatrixReadOnly {
    fn from_parts(m: [f64; 16], is2d: bool) -> DOMMatrixReadOnly {
        DOMMatrixReadOnly { m: std::cell::Cell::new(m), is2d: std::cell::Cell::new(is2d) }
    }
    fn read(&self) -> [f64; 16] { self.m.get() }
    /// The pair every transform kernel takes and returns.
    fn parts(&self) -> Mat { (self.m.get(), self.is2d.get()) }
    fn is_identity(&self) -> bool { self.m.get() == IDENTITY }
}

fn make_matrix(m: [f64; 16], is2d: bool) -> DOMMatrix {
    DOMMatrix { base: DOMMatrixReadOnly::from_parts(m, is2d) }
}

// ---- the transforms themselves ---------------------------------------------------------------
//
// Every geometry operation exists TWICE in the IDL: `scale` on DOMMatrixReadOnly returns a new
// matrix, `scaleSelf` on DOMMatrix applies the same transform to the receiver. Spelling the maths
// once, as `(matrix, is2D) -> (matrix, is2D)`, is what keeps the twins from drifting: the readonly
// method wraps the result in a new object, the mutating one stores it back.

/// A transformation applied about an origin: translate to it, transform, translate back.
fn about_origin(t: [f64; 16], ox: f64, oy: f64, oz: f64) -> [f64; 16] {
    if ox == 0.0 && oy == 0.0 && oz == 0.0 { return t; }
    let mut to = IDENTITY;
    to[12] = ox; to[13] = oy; to[14] = oz;
    let mut back = IDENTITY;
    back[12] = -ox; back[13] = -oy; back[14] = -oz;
    mat_mul(&mat_mul(&to, &t), &back)
}

type Mat = ([f64; 16], bool);

fn tf_translate(m: Mat, tx: Option<f64>, ty: Option<f64>, tz: Option<f64>) -> Mat {
    let (tx, ty, tz) = (tx.unwrap_or(0.), ty.unwrap_or(0.), tz.unwrap_or(0.));
    let mut t = IDENTITY;
    t[12] = tx; t[13] = ty; t[14] = tz;
    (mat_mul(&m.0, &t), m.1 && tz == 0.)
}

/// `scale(sx, sy, sz, originX, originY, originZ)`. `sy` defaults to `sx` (NOT to 1), and a `sz`
/// other than 1 or an `originZ` other than 0 makes the result 3D.
#[allow(clippy::too_many_arguments)]
fn tf_scale(m: Mat, sx: Option<f64>, sy: Option<f64>, sz: Option<f64>, ox: Option<f64>, oy: Option<f64>, oz: Option<f64>) -> Mat {
    let sx = sx.unwrap_or(1.);
    let (sy, sz) = (sy.unwrap_or(sx), sz.unwrap_or(1.));
    let (ox, oy, oz) = (ox.unwrap_or(0.), oy.unwrap_or(0.), oz.unwrap_or(0.));
    let mut t = IDENTITY;
    t[0] = sx; t[5] = sy; t[10] = sz;
    (mat_mul(&m.0, &about_origin(t, ox, oy, oz)), m.1 && sz == 1. && oz == 0.)
}

/// `scale3d(scale, originX, originY, originZ)` — a uniform scale on all three axes. The spec sets
/// is2D false unconditionally, which is why this is not just `tf_scale(s, s, s, …)`.
fn tf_scale3d(m: Mat, s: Option<f64>, ox: Option<f64>, oy: Option<f64>, oz: Option<f64>) -> Mat {
    let s = s.unwrap_or(1.);
    let (ox, oy, oz) = (ox.unwrap_or(0.), oy.unwrap_or(0.), oz.unwrap_or(0.));
    let mut t = IDENTITY;
    t[0] = s; t[5] = s; t[10] = s;
    (mat_mul(&m.0, &about_origin(t, ox, oy, oz)), false)
}

/// `rotate(rotX, rotY, rotZ)`. With ONE argument the angle is the Z rotation (the 2D case); with
/// more, the three are applied Z, then Y, then X.
fn tf_rotate(m: Mat, rx: Option<f64>, ry: Option<f64>, rz: Option<f64>) -> Mat {
    let (rx, ry, rz) = match (ry, rz) {
        (None, None) => (0., 0., rx.unwrap_or(0.)),
        _ => (rx.unwrap_or(0.), ry.unwrap_or(0.), rz.unwrap_or(0.)),
    };
    // Blink's `DOMMatrix::rotateSelf`: Z, then Y, then X, each concatenated in turn, a zero skipped.
    let mut out = m.0;
    if rz != 0. {
        out = mat_mul(&out, &axis_rot(0., 0., 1., rz));
    }
    if ry != 0. {
        out = mat_mul(&out, &axis_rot(0., 1., 0., ry));
    }
    if rx != 0. {
        out = mat_mul(&out, &axis_rot(1., 0., 0., rx));
    }
    (out, m.1 && rx == 0. && ry == 0.)
}

/// `rotateFromVector(x, y)` — rotate by the angle of the vector (x, y); a zero vector is 0deg.
fn tf_rotate_from_vector(m: Mat, x: Option<f64>, y: Option<f64>) -> Mat {
    let (x, y) = (x.unwrap_or(0.), y.unwrap_or(0.));
    let deg = if x == 0. && y == 0. { 0. } else { y.atan2(x) * (180.0 / std::f64::consts::PI) };
    (mat_mul(&m.0, &axis_rot(0., 0., 1., deg)), m.1)
}

fn tf_rotate_axis_angle(m: Mat, x: Option<f64>, y: Option<f64>, z: Option<f64>, angle: Option<f64>) -> Mat {
    let (x, y, z) = (x.unwrap_or(0.), y.unwrap_or(0.), z.unwrap_or(0.));
    (mat_mul(&m.0, &axis_rot(x, y, z, angle.unwrap_or(0.))), m.1 && x == 0. && y == 0.)
}

fn tf_skew_x(m: Mat, a: Option<f64>) -> Mat {
    let mut t = IDENTITY;
    t[4] = (a.unwrap_or(0.) * std::f64::consts::PI / 180.0).tan();
    (mat_mul(&m.0, &t), m.1)
}

fn tf_skew_y(m: Mat, a: Option<f64>) -> Mat {
    let mut t = IDENTITY;
    t[1] = (a.unwrap_or(0.) * std::f64::consts::PI / 180.0).tan();
    (mat_mul(&m.0, &t), m.1)
}

/// `m` times the validated `other` ([`MatrixLike::fixup`]); `at` is the operation's context for its refusal.
fn tf_multiply(m: Mat, other: Option<MatrixLike>, at: &str) -> Result<Mat, JsErrorBox> {
    let (o, o2d) = validate_and_fixup(&other.unwrap_or_default(), at)?;
    Ok((mat_mul(&m.0, &o), m.1 && o2d))
}

// ===== DOMMatrixReadOnly (migrated to scaffold) =====
pub(crate) trait DOMMatrixReadOnlyBehavior {
   fn construct(_scope: &mut v8::PinScope, _init: v8::Local<v8::Value>) -> Result<DOMMatrixReadOnly, JsErrorBox> where Self: Sized { stub!("DOMMatrixReadOnly::constructor") }
   fn a(&self) -> f64 { stub!("DOMMatrixReadOnly::a") }
   fn b(&self) -> f64 { stub!("DOMMatrixReadOnly::b") }
   fn c(&self) -> f64 { stub!("DOMMatrixReadOnly::c") }
   fn d(&self) -> f64 { stub!("DOMMatrixReadOnly::d") }
   fn e(&self) -> f64 { stub!("DOMMatrixReadOnly::e") }
   fn f(&self) -> f64 { stub!("DOMMatrixReadOnly::f") }
   fn m11(&self) -> f64 { stub!("DOMMatrixReadOnly::m11") }
   fn m12(&self) -> f64 { stub!("DOMMatrixReadOnly::m12") }
   fn m13(&self) -> f64 { stub!("DOMMatrixReadOnly::m13") }
   fn m14(&self) -> f64 { stub!("DOMMatrixReadOnly::m14") }
   fn m21(&self) -> f64 { stub!("DOMMatrixReadOnly::m21") }
   fn m22(&self) -> f64 { stub!("DOMMatrixReadOnly::m22") }
   fn m23(&self) -> f64 { stub!("DOMMatrixReadOnly::m23") }
   fn m24(&self) -> f64 { stub!("DOMMatrixReadOnly::m24") }
   fn m31(&self) -> f64 { stub!("DOMMatrixReadOnly::m31") }
   fn m32(&self) -> f64 { stub!("DOMMatrixReadOnly::m32") }
   fn m33(&self) -> f64 { stub!("DOMMatrixReadOnly::m33") }
   fn m34(&self) -> f64 { stub!("DOMMatrixReadOnly::m34") }
   fn m41(&self) -> f64 { stub!("DOMMatrixReadOnly::m41") }
   fn m42(&self) -> f64 { stub!("DOMMatrixReadOnly::m42") }
   fn m43(&self) -> f64 { stub!("DOMMatrixReadOnly::m43") }
   fn m44(&self) -> f64 { stub!("DOMMatrixReadOnly::m44") }
   fn is_2d(&self) -> bool { stub!("DOMMatrixReadOnly::is2D") }
   fn is_identity_getter(&self) -> bool { stub!("DOMMatrixReadOnly::isIdentity") }
   fn translate(&self, _tx: Option<f64>, _ty: Option<f64>, _tz: Option<f64>) -> DOMMatrix { stub!("DOMMatrixReadOnly::translate") }
   fn scale(&self, _sx: Option<f64>, _sy: Option<f64>, _sz: Option<f64>, _ox: Option<f64>, _oy: Option<f64>, _oz: Option<f64>) -> DOMMatrix { stub!("DOMMatrixReadOnly::scale") }
   fn scale_non_uniform(&self, _sx: Option<f64>, _sy: Option<f64>) -> DOMMatrix { stub!("DOMMatrixReadOnly::scaleNonUniform") }
   fn rotate(&self, _rx: Option<f64>, _ry: Option<f64>, _rz: Option<f64>) -> DOMMatrix { stub!("DOMMatrixReadOnly::rotate") }
   fn multiply(&self, _other: Option<MatrixLike>) -> Result<DOMMatrix, JsErrorBox> { stub!("DOMMatrixReadOnly::multiply") }
   fn inverse(&self) -> DOMMatrix { stub!("DOMMatrixReadOnly::inverse") }
   fn skew_x(&self, _a: Option<f64>) -> DOMMatrix { stub!("DOMMatrixReadOnly::skewX") }
   fn skew_y(&self, _a: Option<f64>) -> DOMMatrix { stub!("DOMMatrixReadOnly::skewY") }
   fn flip_x(&self) -> DOMMatrix { stub!("DOMMatrixReadOnly::flipX") }
   fn flip_y(&self) -> DOMMatrix { stub!("DOMMatrixReadOnly::flipY") }
   fn scale3d(&self, _s: Option<f64>, _ox: Option<f64>, _oy: Option<f64>, _oz: Option<f64>) -> DOMMatrix { stub!("DOMMatrixReadOnly::scale3d") }
   fn rotate_axis_angle(&self, _x: Option<f64>, _y: Option<f64>, _z: Option<f64>, _angle: Option<f64>) -> DOMMatrix { stub!("DOMMatrixReadOnly::rotateAxisAngle") }
   fn rotate_from_vector(&self, _x: Option<f64>, _y: Option<f64>) -> DOMMatrix { stub!("DOMMatrixReadOnly::rotateFromVector") }
   fn transform_point(&self, _p: Option<PointInit>) -> DOMPoint { stub!("DOMMatrixReadOnly::transformPoint") }
   fn to_float32_array(&self) -> Vec<f32> { stub!("DOMMatrixReadOnly::toFloat32Array") }
   fn to_float64_array(&self) -> Vec<f64> { stub!("DOMMatrixReadOnly::toFloat64Array") }
   fn to_string_js(&self) -> Result<String, JsErrorBox> { stub!("DOMMatrixReadOnly::toString") }
   fn to_json(&self) -> MatrixJson { stub!("DOMMatrixReadOnly::toJSON") }
   fn from_matrix(_init: Option<MatrixLike>) -> Result<DOMMatrixReadOnly, JsErrorBox> where Self: Sized { stub!("DOMMatrixReadOnly::fromMatrix") }
   fn from_float32_array(_a: &[f32]) -> Result<DOMMatrixReadOnly, JsErrorBox> where Self: Sized { stub!("DOMMatrixReadOnly::fromFloat32Array") }
   fn from_float64_array(_a: &[f64]) -> Result<DOMMatrixReadOnly, JsErrorBox> where Self: Sized { stub!("DOMMatrixReadOnly::fromFloat64Array") }
}

#[webapi(base, own)]
impl DOMMatrixReadOnly {
    #[constructor]
    #[cppgc]
    #[reentrant]
    fn constructor(scope: &mut v8::PinScope, init: v8::Local<v8::Value>) -> Result<DOMMatrixReadOnly, JsErrorBox> { dispatch_try!("DOMMatrixReadOnly::constructor", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::construct(scope, init)) }
    #[getter]
    fn a(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::a", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::a(self)) }
    #[getter]
    fn b(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::b", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::b(self)) }
    #[getter]
    fn c(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::c", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::c(self)) }
    #[getter]
    fn d(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::d", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::d(self)) }
    #[getter]
    fn e(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::e", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::e(self)) }
    #[getter]
    fn f(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::f", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::f(self)) }
    #[getter]
    fn m11(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::m11", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::m11(self)) }
    #[getter]
    fn m12(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::m12", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::m12(self)) }
    #[getter]
    fn m13(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::m13", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::m13(self)) }
    #[getter]
    fn m14(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::m14", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::m14(self)) }
    #[getter]
    fn m21(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::m21", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::m21(self)) }
    #[getter]
    fn m22(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::m22", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::m22(self)) }
    #[getter]
    fn m23(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::m23", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::m23(self)) }
    #[getter]
    fn m24(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::m24", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::m24(self)) }
    #[getter]
    fn m31(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::m31", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::m31(self)) }
    #[getter]
    fn m32(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::m32", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::m32(self)) }
    #[getter]
    fn m33(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::m33", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::m33(self)) }
    #[getter]
    fn m34(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::m34", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::m34(self)) }
    #[getter]
    fn m41(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::m41", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::m41(self)) }
    #[getter]
    fn m42(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::m42", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::m42(self)) }
    #[getter]
    fn m43(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::m43", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::m43(self)) }
    #[getter]
    fn m44(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::m44", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::m44(self)) }
    #[getter]
    #[rename("is2D")]
    fn is_2d(&self) -> Result<bool, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::is2D", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::is_2d(self)) }
    #[getter]
    #[rename("isIdentity")]
    fn is_identity_getter(&self) -> Result<bool, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::isIdentity", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::is_identity_getter(self)) }
    // NOTE: the order of the members below is load-bearing — deno_core registers prototype
    // members in impl order, so this IS the JS enumeration order. V8 exposes interface members
    // alphabetically (see the brave-151 baseline), so keep accessors a..f, m11..m44, is2D,
    // isIdentity, then the methods + statics alphabetically; the quality gate's `order_ok`
    // structure check faults any drift.
    #[rename("flipX")]
    #[cppgc]
    fn flip_x(&self) -> Result<DOMMatrix, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::flipX", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::flip_x(self)) }
    #[rename("flipY")]
    #[cppgc]
    fn flip_y(&self) -> Result<DOMMatrix, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::flipY", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::flip_y(self)) }
    #[cppgc]
    fn inverse(&self) -> Result<DOMMatrix, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::inverse", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::inverse(self)) }
    #[reentrant]
    #[cppgc]
    fn multiply(&self, #[serde] #[dict("DOMMatrixInit")] other: Option<MatrixLike>) -> Result<DOMMatrix, ::deno_error::JsErrorBox> { dispatch_try!("DOMMatrixReadOnly::multiply", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::multiply(self, other)) }
    #[cppgc]
    fn rotate(&self, rx: Option<f64>, ry: Option<f64>, rz: Option<f64>) -> Result<DOMMatrix, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::rotate", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::rotate(self, rx, ry, rz)) }
    #[rename("rotateAxisAngle")]
    #[cppgc]
    fn rotate_axis_angle(&self, x: Option<f64>, y: Option<f64>, z: Option<f64>, angle: Option<f64>) -> Result<DOMMatrix, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::rotateAxisAngle", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::rotate_axis_angle(self, x, y, z, angle)) }
    #[rename("rotateFromVector")]
    #[cppgc]
    fn rotate_from_vector(&self, x: Option<f64>, y: Option<f64>) -> Result<DOMMatrix, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::rotateFromVector", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::rotate_from_vector(self, x, y)) }
    #[cppgc]
    fn scale(&self, sx: Option<f64>, sy: Option<f64>, sz: Option<f64>, ox: Option<f64>, oy: Option<f64>, oz: Option<f64>) -> Result<DOMMatrix, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::scale", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::scale(self, sx, sy, sz, ox, oy, oz)) }
    #[rename("scale3d")]
    #[cppgc]
    fn scale3d(&self, s: Option<f64>, ox: Option<f64>, oy: Option<f64>, oz: Option<f64>) -> Result<DOMMatrix, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::scale3d", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::scale3d(self, s, ox, oy, oz)) }
    #[rename("scaleNonUniform")]
    #[cppgc]
    fn scale_non_uniform(&self, sx: Option<f64>, sy: Option<f64>) -> Result<DOMMatrix, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::scaleNonUniform", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::scale_non_uniform(self, sx, sy)) }
    #[rename("skewX")]
    #[cppgc]
    fn skew_x(&self, a: Option<f64>) -> Result<DOMMatrix, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::skewX", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::skew_x(self, a)) }
    #[rename("skewY")]
    #[cppgc]
    fn skew_y(&self, a: Option<f64>) -> Result<DOMMatrix, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::skewY", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::skew_y(self, a)) }
    #[rename("toFloat32Array")]
    #[buffer]
    fn to_float32_array(&self) -> Result<Vec<f32>, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::toFloat32Array", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::to_float32_array(self)) }
    #[rename("toFloat64Array")]
    #[buffer]
    fn to_float64_array(&self) -> Result<Vec<f64>, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::toFloat64Array", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::to_float64_array(self)) }
    #[rename("toJSON")]
    #[object]
    fn to_json(&self) -> Result<MatrixJson, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::toJSON", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::to_json(self)) }
    #[reentrant]
    #[rename("transformPoint")]
    #[cppgc]
    fn transform_point(&self, #[serde] #[dict("DOMPointInit")] p: Option<PointInit>) -> Result<DOMPoint, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::transformPoint", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::transform_point(self, p)) }
    #[cppgc]
    fn translate(&self, tx: Option<f64>, ty: Option<f64>, tz: Option<f64>) -> Result<DOMMatrix, ::deno_error::JsErrorBox> { dispatch!("DOMMatrixReadOnly::translate", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::translate(self, tx, ty, tz)) }
    #[rename("toString")]
    #[string]
    fn to_string_js(&self) -> Result<String, JsErrorBox> { dispatch_try!("DOMMatrixReadOnly::toString", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::to_string_js(self)) }
    #[rename("fromFloat32Array")]
    #[static_method]
    #[cppgc]
    fn from_float32_array(#[buffer] a: &[f32]) -> Result<DOMMatrixReadOnly, JsErrorBox> { dispatch_try!("DOMMatrixReadOnly::fromFloat32Array", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::from_float32_array(a)) }
    #[rename("fromFloat64Array")]
    #[static_method]
    #[cppgc]
    fn from_float64_array(#[buffer] a: &[f64]) -> Result<DOMMatrixReadOnly, JsErrorBox> { dispatch_try!("DOMMatrixReadOnly::fromFloat64Array", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::from_float64_array(a)) }
    #[cppgc]
    #[reentrant]
    #[rename("fromMatrix")]
    #[static_method]
    fn from_matrix(#[serde] #[dict("DOMMatrixInit")] init: Option<MatrixLike>) -> Result<DOMMatrixReadOnly, JsErrorBox> { dispatch_try!("DOMMatrixReadOnly::fromMatrix", <DOMMatrixReadOnly as DOMMatrixReadOnlyBehavior>::from_matrix(init)) }
}

impl DOMMatrixReadOnlyBehavior for DOMMatrixReadOnly {
    fn construct(scope: &mut v8::PinScope, init: v8::Local<v8::Value>) -> Result<DOMMatrixReadOnly, JsErrorBox> { let (m, is2d) = resolve_ctor_arg(scope, init, "DOMMatrixReadOnly")?;
        Ok(DOMMatrixReadOnly::from_parts(m, is2d)) }
    fn a(&self) -> f64 { self.read()[0] }
    fn b(&self) -> f64 { self.read()[1] }
    fn c(&self) -> f64 { self.read()[4] }
    fn d(&self) -> f64 { self.read()[5] }
    fn e(&self) -> f64 { self.read()[12] }
    fn f(&self) -> f64 { self.read()[13] }
    fn m11(&self) -> f64 { self.read()[0] }
    fn m12(&self) -> f64 { self.read()[1] }
    fn m13(&self) -> f64 { self.read()[2] }
    fn m14(&self) -> f64 { self.read()[3] }
    fn m21(&self) -> f64 { self.read()[4] }
    fn m22(&self) -> f64 { self.read()[5] }
    fn m23(&self) -> f64 { self.read()[6] }
    fn m24(&self) -> f64 { self.read()[7] }
    fn m31(&self) -> f64 { self.read()[8] }
    fn m32(&self) -> f64 { self.read()[9] }
    fn m33(&self) -> f64 { self.read()[10] }
    fn m34(&self) -> f64 { self.read()[11] }
    fn m41(&self) -> f64 { self.read()[12] }
    fn m42(&self) -> f64 { self.read()[13] }
    fn m43(&self) -> f64 { self.read()[14] }
    fn m44(&self) -> f64 { self.read()[15] }
    fn is_2d(&self) -> bool { self.is2d.get() }
    fn is_identity_getter(&self) -> bool { self.is_identity() }
    fn translate(&self, tx: Option<f64>, ty: Option<f64>, tz: Option<f64>) -> DOMMatrix { let (m, d) = tf_translate(self.parts(), tx, ty, tz); make_matrix(m, d) }
    fn scale(&self, sx: Option<f64>, sy: Option<f64>, sz: Option<f64>, ox: Option<f64>, oy: Option<f64>, oz: Option<f64>) -> DOMMatrix { let (m, d) = tf_scale(self.parts(), sx, sy, sz, ox, oy, oz); make_matrix(m, d) }
    // scaleNonUniform is NOT scale(sx, sy): its `sy` defaults to 1, where scale's defaults to `sx`.
    fn scale_non_uniform(&self, sx: Option<f64>, sy: Option<f64>) -> DOMMatrix { let mut t = IDENTITY; t[0] = sx.unwrap_or(1.); t[5] = sy.unwrap_or(1.);
        make_matrix(mat_mul(&self.read(), &t), self.is2d.get()) }
    fn rotate(&self, rx: Option<f64>, ry: Option<f64>, rz: Option<f64>) -> DOMMatrix { let (m, d) = tf_rotate(self.parts(), rx, ry, rz); make_matrix(m, d) }
    fn multiply(&self, other: Option<MatrixLike>) -> Result<DOMMatrix, JsErrorBox> { let (m, d) = tf_multiply(self.parts(), other, "Failed to execute 'multiply' on 'DOMMatrixReadOnly': ")?; Ok(make_matrix(m, d)) }
    /// `inverse()` is defined as "copy the matrix, then perform invertSelf() on the copy", and
    /// `invertSelf()` says that a matrix which cannot be inverted has ALL of its attributes set to
    /// NaN *and its is2D set to false*. So a singular matrix's inverse is not 2D, whatever the
    /// original was. This used to carry `self.is2d` through unchanged, which made the pair disagree:
    /// `new DOMMatrix().scale(0).inverse().is2D` said true while the same matrix through
    /// `invertSelf()` said false. Nothing in the corpus covers it -- it was found by an auditor
    /// running both halves of the twin and comparing.
    fn inverse(&self) -> DOMMatrix { match mat_invert(&self.read()) {
        Some(inv) => make_matrix(inv, self.is2d.get()),
        None => make_matrix([f64::NAN; 16], false),
    } }
    fn skew_x(&self, a: Option<f64>) -> DOMMatrix { let (m, d) = tf_skew_x(self.parts(), a); make_matrix(m, d) }
    fn skew_y(&self, a: Option<f64>) -> DOMMatrix { let (m, d) = tf_skew_y(self.parts(), a); make_matrix(m, d) }
    fn flip_x(&self) -> DOMMatrix { make_matrix(mat_mul(&self.read(), &[-1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.]), self.is2d.get()) }
    fn flip_y(&self) -> DOMMatrix { make_matrix(mat_mul(&self.read(), &[1., 0., 0., 0., 0., -1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.]), self.is2d.get()) }
    fn scale3d(&self, s: Option<f64>, ox: Option<f64>, oy: Option<f64>, oz: Option<f64>) -> DOMMatrix { let (m, d) = tf_scale3d(self.parts(), s, ox, oy, oz); make_matrix(m, d) }
    fn rotate_axis_angle(&self, x: Option<f64>, y: Option<f64>, z: Option<f64>, angle: Option<f64>) -> DOMMatrix { let (m, d) = tf_rotate_axis_angle(self.parts(), x, y, z, angle); make_matrix(m, d) }
    fn rotate_from_vector(&self, x: Option<f64>, y: Option<f64>) -> DOMMatrix { let (m, d) = tf_rotate_from_vector(self.parts(), x, y); make_matrix(m, d) }
    fn transform_point(&self, p: Option<PointInit>) -> DOMPoint { let p = p.unwrap_or_default();
        let m = self.read();
        DOMPoint { base: DOMPointReadOnly::make(
            m[0]*p.x + m[4]*p.y + m[8]*p.z + m[12]*p.w,
            m[1]*p.x + m[5]*p.y + m[9]*p.z + m[13]*p.w,
            m[2]*p.x + m[6]*p.y + m[10]*p.z + m[14]*p.w,
            m[3]*p.x + m[7]*p.y + m[11]*p.z + m[15]*p.w,
        ) } }
    fn to_float32_array(&self) -> Vec<f32> { self.read().iter().map(|&x| x as f32).collect() }
    fn to_float64_array(&self) -> Vec<f64> { self.read().to_vec() }
    fn to_string_js(&self) -> Result<String, JsErrorBox> { let m = self.read();
        if !m.iter().all(|x| x.is_finite()) { return Err(JsErrorBox::new("InvalidStateError", "matrix has non-finite values")); }
        if self.is2d.get() {
            Ok(format!("matrix({}, {}, {}, {}, {}, {})", js_number(m[0]), js_number(m[1]), js_number(m[4]), js_number(m[5]), js_number(m[12]), js_number(m[13])))
        } else {
            let parts: Vec<String> = m.iter().map(|x| js_number(*x)).collect();
            Ok(format!("matrix3d({})", parts.join(", ")))
        } }
    fn to_json(&self) -> MatrixJson { let m = self.read();
        MatrixJson {
            a: m[0], b: m[1], c: m[4], d: m[5], e: m[12], f: m[13],
            m11: m[0], m12: m[1], m13: m[2], m14: m[3], m21: m[4], m22: m[5], m23: m[6], m24: m[7],
            m31: m[8], m32: m[9], m33: m[10], m34: m[11], m41: m[12], m42: m[13], m43: m[14], m44: m[15],
            is_2d: self.is2d.get(), is_identity: self.is_identity(),
        } }
    fn from_matrix(init: Option<MatrixLike>) -> Result<DOMMatrixReadOnly, JsErrorBox> { let (m, is2d) = validate_and_fixup(&init.unwrap_or_default(), "Failed to execute 'fromMatrix' on 'DOMMatrixReadOnly': ")?;
        Ok(DOMMatrixReadOnly::from_parts(m, is2d)) }
    fn from_float32_array(a: &[f32]) -> Result<DOMMatrixReadOnly, JsErrorBox> { let nums: Vec<f64> = a.iter().map(|&x| x as f64).collect();
        let (m, is2d) = matrix_from_array(&nums)?;
        Ok(DOMMatrixReadOnly::from_parts(m, is2d)) }
    fn from_float64_array(a: &[f64]) -> Result<DOMMatrixReadOnly, JsErrorBox> { let (m, is2d) = matrix_from_array(a)?;
        Ok(DOMMatrixReadOnly::from_parts(m, is2d)) }
}

#[repr(C)]
#[derive(CppgcInherits, CppgcBase)]
#[cppgc_inherits_from(DOMMatrixReadOnly)]
pub struct DOMMatrix {
    base: DOMMatrixReadOnly,
}

impl DOMMatrix {
    fn set_elem(&self, idx: usize, v: f64) {
        let mut m = self.base.m.get();
        m[idx] = v;
        self.base.m.set(m);
        // Setting an off-identity 3D element clears is2D (2D indices: 0,1,4,5,12,13).
        let is_2d_index = matches!(idx, 0 | 1 | 4 | 5 | 12 | 13);
        if !is_2d_index && !(v == IDENTITY[idx] || (v == 0.0 && IDENTITY[idx] == 0.0)) {
            self.base.is2d.set(false);
        }
    }
    fn mutate(&self, m: [f64; 16]) { self.base.m.set(m); }
    /// Store back what a transform kernel produced -- the mutating half of `make_matrix`.
    fn apply(&self, r: Mat) { self.base.m.set(r.0); self.base.is2d.set(r.1); }
}

// ===== DOMMatrix (migrated to scaffold) =====
pub(crate) trait DOMMatrixBehavior {
   fn construct(_scope: &mut v8::PinScope, _init: v8::Local<v8::Value>) -> Result<DOMMatrix, JsErrorBox> where Self: Sized { stub!("DOMMatrix::constructor") }
   fn a(&self) -> f64 { stub!("DOMMatrix::a") }
   fn set_a(&self, _v: f64)  { stub!("DOMMatrix::a=") }
   fn b(&self) -> f64 { stub!("DOMMatrix::b") }
   fn set_b(&self, _v: f64)  { stub!("DOMMatrix::b=") }
   fn c(&self) -> f64 { stub!("DOMMatrix::c") }
   fn set_c(&self, _v: f64)  { stub!("DOMMatrix::c=") }
   fn d(&self) -> f64 { stub!("DOMMatrix::d") }
   fn set_d(&self, _v: f64)  { stub!("DOMMatrix::d=") }
   fn e(&self) -> f64 { stub!("DOMMatrix::e") }
   fn set_e(&self, _v: f64)  { stub!("DOMMatrix::e=") }
   fn f(&self) -> f64 { stub!("DOMMatrix::f") }
   fn set_f(&self, _v: f64)  { stub!("DOMMatrix::f=") }
   fn m11(&self) -> f64 { stub!("DOMMatrix::m11") }
   fn set_m11(&self, _v: f64)  { stub!("DOMMatrix::m11=") }
   fn m12(&self) -> f64 { stub!("DOMMatrix::m12") }
   fn set_m12(&self, _v: f64)  { stub!("DOMMatrix::m12=") }
   fn m13(&self) -> f64 { stub!("DOMMatrix::m13") }
   fn set_m13(&self, _v: f64)  { stub!("DOMMatrix::m13=") }
   fn m14(&self) -> f64 { stub!("DOMMatrix::m14") }
   fn set_m14(&self, _v: f64)  { stub!("DOMMatrix::m14=") }
   fn m21(&self) -> f64 { stub!("DOMMatrix::m21") }
   fn set_m21(&self, _v: f64)  { stub!("DOMMatrix::m21=") }
   fn m22(&self) -> f64 { stub!("DOMMatrix::m22") }
   fn set_m22(&self, _v: f64)  { stub!("DOMMatrix::m22=") }
   fn m23(&self) -> f64 { stub!("DOMMatrix::m23") }
   fn set_m23(&self, _v: f64)  { stub!("DOMMatrix::m23=") }
   fn m24(&self) -> f64 { stub!("DOMMatrix::m24") }
   fn set_m24(&self, _v: f64)  { stub!("DOMMatrix::m24=") }
   fn m31(&self) -> f64 { stub!("DOMMatrix::m31") }
   fn set_m31(&self, _v: f64)  { stub!("DOMMatrix::m31=") }
   fn m32(&self) -> f64 { stub!("DOMMatrix::m32") }
   fn set_m32(&self, _v: f64)  { stub!("DOMMatrix::m32=") }
   fn m33(&self) -> f64 { stub!("DOMMatrix::m33") }
   fn set_m33(&self, _v: f64)  { stub!("DOMMatrix::m33=") }
   fn m34(&self) -> f64 { stub!("DOMMatrix::m34") }
   fn set_m34(&self, _v: f64)  { stub!("DOMMatrix::m34=") }
   fn m41(&self) -> f64 { stub!("DOMMatrix::m41") }
   fn set_m41(&self, _v: f64)  { stub!("DOMMatrix::m41=") }
   fn m42(&self) -> f64 { stub!("DOMMatrix::m42") }
   fn set_m42(&self, _v: f64)  { stub!("DOMMatrix::m42=") }
   fn m43(&self) -> f64 { stub!("DOMMatrix::m43") }
   fn set_m43(&self, _v: f64)  { stub!("DOMMatrix::m43=") }
   fn m44(&self) -> f64 { stub!("DOMMatrix::m44") }
   fn set_m44(&self, _v: f64)  { stub!("DOMMatrix::m44=") }
   fn invert_self(&self, _this: v8::Global<v8::Object>) -> v8::Global<v8::Object> { stub!("DOMMatrix::invertSelf") }
   fn multiply_self(&self, _this: v8::Global<v8::Object>, _other: Option<MatrixLike>) -> Result<v8::Global<v8::Object>, JsErrorBox> { stub!("DOMMatrix::multiplySelf") }
   fn pre_multiply_self(&self, _this: v8::Global<v8::Object>, _other: Option<MatrixLike>) -> Result<v8::Global<v8::Object>, JsErrorBox> { stub!("DOMMatrix::preMultiplySelf") }
   fn rotate_axis_angle_self(&self, _this: v8::Global<v8::Object>, _x: Option<f64>, _y: Option<f64>, _z: Option<f64>, _angle: Option<f64>) -> v8::Global<v8::Object> { stub!("DOMMatrix::rotateAxisAngleSelf") }
   fn rotate_from_vector_self(&self, _this: v8::Global<v8::Object>, _x: Option<f64>, _y: Option<f64>) -> v8::Global<v8::Object> { stub!("DOMMatrix::rotateFromVectorSelf") }
   fn rotate_self(&self, _this: v8::Global<v8::Object>, _rx: Option<f64>, _ry: Option<f64>, _rz: Option<f64>) -> v8::Global<v8::Object> { stub!("DOMMatrix::rotateSelf") }
   fn scale3d_self(&self, _this: v8::Global<v8::Object>, _s: Option<f64>, _ox: Option<f64>, _oy: Option<f64>, _oz: Option<f64>) -> v8::Global<v8::Object> { stub!("DOMMatrix::scale3dSelf") }
   fn scale_self(&self, _this: v8::Global<v8::Object>, _sx: Option<f64>, _sy: Option<f64>, _sz: Option<f64>, _ox: Option<f64>, _oy: Option<f64>, _oz: Option<f64>) -> v8::Global<v8::Object> { stub!("DOMMatrix::scaleSelf") }
   fn skew_x_self(&self, _this: v8::Global<v8::Object>, _a: Option<f64>) -> v8::Global<v8::Object> { stub!("DOMMatrix::skewXSelf") }
   fn skew_y_self(&self, _this: v8::Global<v8::Object>, _a: Option<f64>) -> v8::Global<v8::Object> { stub!("DOMMatrix::skewYSelf") }
   fn translate_self(&self, _this: v8::Global<v8::Object>, _tx: Option<f64>, _ty: Option<f64>, _tz: Option<f64>) -> v8::Global<v8::Object> { stub!("DOMMatrix::translateSelf") }
   fn set_matrix_value(&self, _this: v8::Global<v8::Object>, _s: String) -> Result<v8::Global<v8::Object>, JsErrorBox> { stub!("DOMMatrix::setMatrixValue") }
   fn from_matrix(_init: Option<MatrixLike>) -> Result<DOMMatrix, JsErrorBox> where Self: Sized { stub!("DOMMatrix::fromMatrix") }
   fn from_float32_array(_a: &[f32]) -> Result<DOMMatrix, JsErrorBox> where Self: Sized { stub!("DOMMatrix::fromFloat32Array") }
   fn from_float64_array(_a: &[f64]) -> Result<DOMMatrix, JsErrorBox> where Self: Sized { stub!("DOMMatrix::fromFloat64Array") }
}

#[webapi(inherit = DOMMatrixReadOnly, own)]
impl DOMMatrix {
    #[constructor]
    #[cppgc]
    #[reentrant]
    fn constructor(scope: &mut v8::PinScope, init: v8::Local<v8::Value>) -> Result<DOMMatrix, JsErrorBox> { dispatch_try!("DOMMatrix::constructor", <DOMMatrix as DOMMatrixBehavior>::construct(scope, init)) }
    #[getter]
    fn a(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::a", <DOMMatrix as DOMMatrixBehavior>::a(self)) }
    #[setter]
    fn a(&self, v: f64) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::a=", <DOMMatrix as DOMMatrixBehavior>::set_a(self, v)) }
    #[getter]
    fn b(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::b", <DOMMatrix as DOMMatrixBehavior>::b(self)) }
    #[setter]
    fn b(&self, v: f64) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::b=", <DOMMatrix as DOMMatrixBehavior>::set_b(self, v)) }
    #[getter]
    fn c(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::c", <DOMMatrix as DOMMatrixBehavior>::c(self)) }
    #[setter]
    fn c(&self, v: f64) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::c=", <DOMMatrix as DOMMatrixBehavior>::set_c(self, v)) }
    #[getter]
    fn d(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::d", <DOMMatrix as DOMMatrixBehavior>::d(self)) }
    #[setter]
    fn d(&self, v: f64) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::d=", <DOMMatrix as DOMMatrixBehavior>::set_d(self, v)) }
    #[getter]
    fn e(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::e", <DOMMatrix as DOMMatrixBehavior>::e(self)) }
    #[setter]
    fn e(&self, v: f64) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::e=", <DOMMatrix as DOMMatrixBehavior>::set_e(self, v)) }
    #[getter]
    fn f(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::f", <DOMMatrix as DOMMatrixBehavior>::f(self)) }
    #[setter]
    fn f(&self, v: f64) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::f=", <DOMMatrix as DOMMatrixBehavior>::set_f(self, v)) }
    #[getter]
    fn m11(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m11", <DOMMatrix as DOMMatrixBehavior>::m11(self)) }
    #[setter]
    fn m11(&self, v: f64) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m11=", <DOMMatrix as DOMMatrixBehavior>::set_m11(self, v)) }
    #[getter]
    fn m12(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m12", <DOMMatrix as DOMMatrixBehavior>::m12(self)) }
    #[setter]
    fn m12(&self, v: f64) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m12=", <DOMMatrix as DOMMatrixBehavior>::set_m12(self, v)) }
    #[getter]
    fn m13(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m13", <DOMMatrix as DOMMatrixBehavior>::m13(self)) }
    #[setter]
    fn m13(&self, v: f64) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m13=", <DOMMatrix as DOMMatrixBehavior>::set_m13(self, v)) }
    #[getter]
    fn m14(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m14", <DOMMatrix as DOMMatrixBehavior>::m14(self)) }
    #[setter]
    fn m14(&self, v: f64) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m14=", <DOMMatrix as DOMMatrixBehavior>::set_m14(self, v)) }
    #[getter]
    fn m21(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m21", <DOMMatrix as DOMMatrixBehavior>::m21(self)) }
    #[setter]
    fn m21(&self, v: f64) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m21=", <DOMMatrix as DOMMatrixBehavior>::set_m21(self, v)) }
    #[getter]
    fn m22(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m22", <DOMMatrix as DOMMatrixBehavior>::m22(self)) }
    #[setter]
    fn m22(&self, v: f64) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m22=", <DOMMatrix as DOMMatrixBehavior>::set_m22(self, v)) }
    #[getter]
    fn m23(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m23", <DOMMatrix as DOMMatrixBehavior>::m23(self)) }
    #[setter]
    fn m23(&self, v: f64) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m23=", <DOMMatrix as DOMMatrixBehavior>::set_m23(self, v)) }
    #[getter]
    fn m24(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m24", <DOMMatrix as DOMMatrixBehavior>::m24(self)) }
    #[setter]
    fn m24(&self, v: f64) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m24=", <DOMMatrix as DOMMatrixBehavior>::set_m24(self, v)) }
    #[getter]
    fn m31(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m31", <DOMMatrix as DOMMatrixBehavior>::m31(self)) }
    #[setter]
    fn m31(&self, v: f64) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m31=", <DOMMatrix as DOMMatrixBehavior>::set_m31(self, v)) }
    #[getter]
    fn m32(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m32", <DOMMatrix as DOMMatrixBehavior>::m32(self)) }
    #[setter]
    fn m32(&self, v: f64) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m32=", <DOMMatrix as DOMMatrixBehavior>::set_m32(self, v)) }
    #[getter]
    fn m33(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m33", <DOMMatrix as DOMMatrixBehavior>::m33(self)) }
    #[setter]
    fn m33(&self, v: f64) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m33=", <DOMMatrix as DOMMatrixBehavior>::set_m33(self, v)) }
    #[getter]
    fn m34(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m34", <DOMMatrix as DOMMatrixBehavior>::m34(self)) }
    #[setter]
    fn m34(&self, v: f64) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m34=", <DOMMatrix as DOMMatrixBehavior>::set_m34(self, v)) }
    #[getter]
    fn m41(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m41", <DOMMatrix as DOMMatrixBehavior>::m41(self)) }
    #[setter]
    fn m41(&self, v: f64) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m41=", <DOMMatrix as DOMMatrixBehavior>::set_m41(self, v)) }
    #[getter]
    fn m42(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m42", <DOMMatrix as DOMMatrixBehavior>::m42(self)) }
    #[setter]
    fn m42(&self, v: f64) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m42=", <DOMMatrix as DOMMatrixBehavior>::set_m42(self, v)) }
    #[getter]
    fn m43(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m43", <DOMMatrix as DOMMatrixBehavior>::m43(self)) }
    #[setter]
    fn m43(&self, v: f64) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m43=", <DOMMatrix as DOMMatrixBehavior>::set_m43(self, v)) }
    #[getter]
    fn m44(&self) -> Result<f64, ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m44", <DOMMatrix as DOMMatrixBehavior>::m44(self)) }
    #[setter]
    fn m44(&self, v: f64) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::m44=", <DOMMatrix as DOMMatrixBehavior>::set_m44(self, v)) }
    // NOTE: the order below is load-bearing -- deno_core registers prototype members in impl order,
    // so this IS the JS enumeration order. Chrome (brave-151 baseline) lists the mutating methods
    // alphabetically and then `setMatrixValue` (the CSSOM-legacy member) last; the statics are
    // alphabetical too. The quality gate's structure check faults any drift.
    #[rename("invertSelf")]
    fn invert_self(&self, #[this] this: v8::Global<v8::Object>) -> Result<v8::Global<v8::Object>, ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::invertSelf", <DOMMatrix as DOMMatrixBehavior>::invert_self(self, this)) }
    #[reentrant]
    #[rename("multiplySelf")]
    fn multiply_self(&self, #[this] this: v8::Global<v8::Object>, #[serde] #[dict("DOMMatrixInit")] other: Option<MatrixLike>) -> Result<v8::Global<v8::Object>, ::deno_error::JsErrorBox> { dispatch_try!("DOMMatrix::multiplySelf", <DOMMatrix as DOMMatrixBehavior>::multiply_self(self, this, other)) }
    #[reentrant]
    #[rename("preMultiplySelf")]
    fn pre_multiply_self(&self, #[this] this: v8::Global<v8::Object>, #[serde] #[dict("DOMMatrixInit")] other: Option<MatrixLike>) -> Result<v8::Global<v8::Object>, ::deno_error::JsErrorBox> { dispatch_try!("DOMMatrix::preMultiplySelf", <DOMMatrix as DOMMatrixBehavior>::pre_multiply_self(self, this, other)) }
    #[rename("rotateAxisAngleSelf")]
    fn rotate_axis_angle_self(&self, #[this] this: v8::Global<v8::Object>, x: Option<f64>, y: Option<f64>, z: Option<f64>, angle: Option<f64>) -> Result<v8::Global<v8::Object>, ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::rotateAxisAngleSelf", <DOMMatrix as DOMMatrixBehavior>::rotate_axis_angle_self(self, this, x, y, z, angle)) }
    #[rename("rotateFromVectorSelf")]
    fn rotate_from_vector_self(&self, #[this] this: v8::Global<v8::Object>, x: Option<f64>, y: Option<f64>) -> Result<v8::Global<v8::Object>, ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::rotateFromVectorSelf", <DOMMatrix as DOMMatrixBehavior>::rotate_from_vector_self(self, this, x, y)) }
    #[rename("rotateSelf")]
    fn rotate_self(&self, #[this] this: v8::Global<v8::Object>, rx: Option<f64>, ry: Option<f64>, rz: Option<f64>) -> Result<v8::Global<v8::Object>, ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::rotateSelf", <DOMMatrix as DOMMatrixBehavior>::rotate_self(self, this, rx, ry, rz)) }
    #[rename("scale3dSelf")]
    fn scale3d_self(&self, #[this] this: v8::Global<v8::Object>, s: Option<f64>, ox: Option<f64>, oy: Option<f64>, oz: Option<f64>) -> Result<v8::Global<v8::Object>, ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::scale3dSelf", <DOMMatrix as DOMMatrixBehavior>::scale3d_self(self, this, s, ox, oy, oz)) }
    #[rename("scaleSelf")]
    fn scale_self(&self, #[this] this: v8::Global<v8::Object>, sx: Option<f64>, sy: Option<f64>, sz: Option<f64>, ox: Option<f64>, oy: Option<f64>, oz: Option<f64>) -> Result<v8::Global<v8::Object>, ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::scaleSelf", <DOMMatrix as DOMMatrixBehavior>::scale_self(self, this, sx, sy, sz, ox, oy, oz)) }
    #[rename("skewXSelf")]
    fn skew_x_self(&self, #[this] this: v8::Global<v8::Object>, a: Option<f64>) -> Result<v8::Global<v8::Object>, ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::skewXSelf", <DOMMatrix as DOMMatrixBehavior>::skew_x_self(self, this, a)) }
    #[rename("skewYSelf")]
    fn skew_y_self(&self, #[this] this: v8::Global<v8::Object>, a: Option<f64>) -> Result<v8::Global<v8::Object>, ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::skewYSelf", <DOMMatrix as DOMMatrixBehavior>::skew_y_self(self, this, a)) }
    #[rename("translateSelf")]
    fn translate_self(&self, #[this] this: v8::Global<v8::Object>, tx: Option<f64>, ty: Option<f64>, tz: Option<f64>) -> Result<v8::Global<v8::Object>, ::deno_error::JsErrorBox> { dispatch!("DOMMatrix::translateSelf", <DOMMatrix as DOMMatrixBehavior>::translate_self(self, this, tx, ty, tz)) }
    #[rename("setMatrixValue")]
    fn set_matrix_value(&self, #[this] this: v8::Global<v8::Object>, #[string] s: String) -> Result<v8::Global<v8::Object>, JsErrorBox> { dispatch_try!("DOMMatrix::setMatrixValue", <DOMMatrix as DOMMatrixBehavior>::set_matrix_value(self, this, s)) }
    #[rename("fromFloat32Array")]
    #[static_method]
    #[cppgc]
    fn from_float32_array(#[buffer] a: &[f32]) -> Result<DOMMatrix, JsErrorBox> { dispatch_try!("DOMMatrix::fromFloat32Array", <DOMMatrix as DOMMatrixBehavior>::from_float32_array(a)) }
    #[rename("fromFloat64Array")]
    #[static_method]
    #[cppgc]
    fn from_float64_array(#[buffer] a: &[f64]) -> Result<DOMMatrix, JsErrorBox> { dispatch_try!("DOMMatrix::fromFloat64Array", <DOMMatrix as DOMMatrixBehavior>::from_float64_array(a)) }
    #[cppgc]
    #[reentrant]
    #[rename("fromMatrix")]
    #[static_method]
    fn from_matrix(#[serde] #[dict("DOMMatrixInit")] init: Option<MatrixLike>) -> Result<DOMMatrix, JsErrorBox> { dispatch_try!("DOMMatrix::fromMatrix", <DOMMatrix as DOMMatrixBehavior>::from_matrix(init)) }
}

impl DOMMatrixBehavior for DOMMatrix {
    fn construct(scope: &mut v8::PinScope, init: v8::Local<v8::Value>) -> Result<DOMMatrix, JsErrorBox> { let (m, is2d) = resolve_ctor_arg(scope, init, "DOMMatrix")?;
        Ok(make_matrix(m, is2d)) }
    fn a(&self) -> f64 { self.base.read()[0] }
    fn set_a(&self, v: f64)  { self.set_elem(0, v) }
    fn b(&self) -> f64 { self.base.read()[1] }
    fn set_b(&self, v: f64)  { self.set_elem(1, v) }
    fn c(&self) -> f64 { self.base.read()[4] }
    fn set_c(&self, v: f64)  { self.set_elem(4, v) }
    fn d(&self) -> f64 { self.base.read()[5] }
    fn set_d(&self, v: f64)  { self.set_elem(5, v) }
    fn e(&self) -> f64 { self.base.read()[12] }
    fn set_e(&self, v: f64)  { self.set_elem(12, v) }
    fn f(&self) -> f64 { self.base.read()[13] }
    fn set_f(&self, v: f64)  { self.set_elem(13, v) }
    fn m11(&self) -> f64 { self.base.read()[0] }
    fn set_m11(&self, v: f64)  { self.set_elem(0, v) }
    fn m12(&self) -> f64 { self.base.read()[1] }
    fn set_m12(&self, v: f64)  { self.set_elem(1, v) }
    fn m13(&self) -> f64 { self.base.read()[2] }
    fn set_m13(&self, v: f64)  { self.set_elem(2, v) }
    fn m14(&self) -> f64 { self.base.read()[3] }
    fn set_m14(&self, v: f64)  { self.set_elem(3, v) }
    fn m21(&self) -> f64 { self.base.read()[4] }
    fn set_m21(&self, v: f64)  { self.set_elem(4, v) }
    fn m22(&self) -> f64 { self.base.read()[5] }
    fn set_m22(&self, v: f64)  { self.set_elem(5, v) }
    fn m23(&self) -> f64 { self.base.read()[6] }
    fn set_m23(&self, v: f64)  { self.set_elem(6, v) }
    fn m24(&self) -> f64 { self.base.read()[7] }
    fn set_m24(&self, v: f64)  { self.set_elem(7, v) }
    fn m31(&self) -> f64 { self.base.read()[8] }
    fn set_m31(&self, v: f64)  { self.set_elem(8, v) }
    fn m32(&self) -> f64 { self.base.read()[9] }
    fn set_m32(&self, v: f64)  { self.set_elem(9, v) }
    fn m33(&self) -> f64 { self.base.read()[10] }
    fn set_m33(&self, v: f64)  { self.set_elem(10, v) }
    fn m34(&self) -> f64 { self.base.read()[11] }
    fn set_m34(&self, v: f64)  { self.set_elem(11, v) }
    fn m41(&self) -> f64 { self.base.read()[12] }
    fn set_m41(&self, v: f64)  { self.set_elem(12, v) }
    fn m42(&self) -> f64 { self.base.read()[13] }
    fn set_m42(&self, v: f64)  { self.set_elem(13, v) }
    fn m43(&self) -> f64 { self.base.read()[14] }
    fn set_m43(&self, v: f64)  { self.set_elem(14, v) }
    fn m44(&self) -> f64 { self.base.read()[15] }
    fn set_m44(&self, v: f64)  { self.set_elem(15, v) }
    // Each mutator is its readonly twin's transform stored back into the receiver: same kernel,
    // same is2D bookkeeping, `this` returned so the calls chain.
    fn invert_self(&self, this: v8::Global<v8::Object>) -> v8::Global<v8::Object> { match mat_invert(&self.base.read()) {
            Some(inv) => self.mutate(inv),
            None => { self.mutate([f64::NAN; 16]); self.base.is2d.set(false); }
        }
        this }
    fn multiply_self(&self, this: v8::Global<v8::Object>, other: Option<MatrixLike>) -> Result<v8::Global<v8::Object>, JsErrorBox> {
        self.apply(tf_multiply(self.base.parts(), other, "Failed to execute 'multiplySelf' on 'DOMMatrix': ")?);
        Ok(this) }
    fn pre_multiply_self(&self, this: v8::Global<v8::Object>, other: Option<MatrixLike>) -> Result<v8::Global<v8::Object>, JsErrorBox> {
        let (o, o2d) = validate_and_fixup(&other.unwrap_or_default(), "Failed to execute 'preMultiplySelf' on 'DOMMatrix': ")?;
        self.apply((mat_mul(&o, &self.base.read()), self.base.is2d.get() && o2d));
        Ok(this) }
    fn rotate_axis_angle_self(&self, this: v8::Global<v8::Object>, x: Option<f64>, y: Option<f64>, z: Option<f64>, angle: Option<f64>) -> v8::Global<v8::Object> { self.apply(tf_rotate_axis_angle(self.base.parts(), x, y, z, angle)); this }
    fn rotate_from_vector_self(&self, this: v8::Global<v8::Object>, x: Option<f64>, y: Option<f64>) -> v8::Global<v8::Object> { self.apply(tf_rotate_from_vector(self.base.parts(), x, y)); this }
    fn rotate_self(&self, this: v8::Global<v8::Object>, rx: Option<f64>, ry: Option<f64>, rz: Option<f64>) -> v8::Global<v8::Object> { self.apply(tf_rotate(self.base.parts(), rx, ry, rz)); this }
    fn scale3d_self(&self, this: v8::Global<v8::Object>, s: Option<f64>, ox: Option<f64>, oy: Option<f64>, oz: Option<f64>) -> v8::Global<v8::Object> { self.apply(tf_scale3d(self.base.parts(), s, ox, oy, oz)); this }
    fn scale_self(&self, this: v8::Global<v8::Object>, sx: Option<f64>, sy: Option<f64>, sz: Option<f64>, ox: Option<f64>, oy: Option<f64>, oz: Option<f64>) -> v8::Global<v8::Object> { self.apply(tf_scale(self.base.parts(), sx, sy, sz, ox, oy, oz)); this }
    fn skew_x_self(&self, this: v8::Global<v8::Object>, a: Option<f64>) -> v8::Global<v8::Object> { self.apply(tf_skew_x(self.base.parts(), a)); this }
    fn skew_y_self(&self, this: v8::Global<v8::Object>, a: Option<f64>) -> v8::Global<v8::Object> { self.apply(tf_skew_y(self.base.parts(), a)); this }
    fn translate_self(&self, this: v8::Global<v8::Object>, tx: Option<f64>, ty: Option<f64>, tz: Option<f64>) -> v8::Global<v8::Object> { self.apply(tf_translate(self.base.parts(), tx, ty, tz)); this }
    // `DOMMatrix setMatrixValue(DOMString transformList)` -- the IDL return type is the matrix
    // ITSELF, not `undefined` (webref geometry.idl:260, and Chrome 151 measured:
    // `typeof m.setMatrixValue("translate(1px,2px)")` is "object" and it is not undefined). The Rust
    // body returned `()`, so op2 rendered null and `dom/08c-collections.js`'s arity guard then
    // rewrote that null to `undefined` under an `isVoid` flag that was simply wrong -- two layers
    // agreeing on the same mistake. It returns `this`, like every other mutating member here.
    fn set_matrix_value(&self, this: v8::Global<v8::Object>, s: String) -> Result<v8::Global<v8::Object>, JsErrorBox> { let (m, is2d) = parse_transform(&s).map_err(|_| syntax_err(&format!("Failed to execute 'setMatrixValue' on 'DOMMatrix': Failed to parse '{s}'.")))?;
        self.base.m.set(m);
        self.base.is2d.set(is2d);
        Ok(this) }
    fn from_matrix(init: Option<MatrixLike>) -> Result<DOMMatrix, JsErrorBox> { let (m, is2d) = validate_and_fixup(&init.unwrap_or_default(), "Failed to execute 'fromMatrix' on 'DOMMatrix': ")?;
        Ok(make_matrix(m, is2d)) }
    fn from_float32_array(a: &[f32]) -> Result<DOMMatrix, JsErrorBox> { let nums: Vec<f64> = a.iter().map(|&x| x as f64).collect();
        let (m, is2d) = matrix_from_array(&nums)?;
        Ok(make_matrix(m, is2d)) }
    fn from_float64_array(a: &[f64]) -> Result<DOMMatrix, JsErrorBox> { let (m, is2d) = matrix_from_array(a)?;
        Ok(make_matrix(m, is2d)) }
}


// ── One-table definition ─────────────────────────────────────────────────────────
//
// Rows VERBATIM from the `DOMMatrixReadOnly`/`DOMMatrix` blocks of `install/generated_registry.rs`
// (the capture's order, webref's types/lengths/required counts). `deno_core` realises the whole
// interface from the `#[webapi]` blocks above, in the SAME order (a..f, m11..m44, is2D/isIdentity,
// then the methods alphabetically, then the statics), so `install::live::reconcile_dom_matrix_family`
// VERIFIES the rows in place -- the `Symbol.toStringTag`, the WebIDL `.length`s, the constructor
// chain (`DOMMatrix.__proto__ === DOMMatrixReadOnly`, which the deleted `__matrixCtor` Proxy never
// set) and the two conversion layers `finish_native_member` supplies from the row: the arity guard on
// `setMatrixValue`/`fromFloat32Array`/`fromFloat64Array` (`required` here) and the `unrestricted
// double` coercion on `DOMMatrix`'s 22 writable attributes -- are what this call corrects.
//
// `[value, both]`: both interfaces are `[Exposed=(Window,Worker)]`. The capture lists a WINDOW-ONLY
// `toString` (`DOMMatrixReadOnly`) and `setMatrixValue` (`DOMMatrix`) -- twelve such per-member
// exposure differences exist across webref, and today's `interface!` macro states exposure ONCE for
// the whole table (`macros/src/lib.rs`: "the generated registry is where per-member exposure
// differs... when a hand table needs to say it, W1's table syntax adds the word here"). That is
// mechanism 3 this unit's brief named and could not close alone: the worker realm's own cutover
// (a sibling unit, `worker.rs` + the worker JS files) has to either extend that syntax or filter
// these two rows itself before this table can be the worker's DOMMatrix too. Landed here: the WINDOW
// half only (`core/korvus-runtime/src/engine.rs`), which needs no per-row exposure at all --
// `install::live::geometry::verify_family` reads every row through `table_rows`, unfiltered.
webapi::interface! {
    "DOMMatrixReadOnly" [value, both, class DOMMatrixReadOnly] {
        ctor          "DOMMatrixReadOnly"                len 0            = stub,
        attr          "a"                : Double readonly                = cppgc,
        attr          "b"                : Double readonly                = cppgc,
        attr          "c"                : Double readonly                = cppgc,
        attr          "d"                : Double readonly                = cppgc,
        attr          "e"                : Double readonly                = cppgc,
        attr          "f"                : Double readonly                = cppgc,
        attr          "m11"              : Double readonly                = cppgc,
        attr          "m12"              : Double readonly                = cppgc,
        attr          "m13"              : Double readonly                = cppgc,
        attr          "m14"              : Double readonly                = cppgc,
        attr          "m21"              : Double readonly                = cppgc,
        attr          "m22"              : Double readonly                = cppgc,
        attr          "m23"              : Double readonly                = cppgc,
        attr          "m24"              : Double readonly                = cppgc,
        attr          "m31"              : Double readonly                = cppgc,
        attr          "m32"              : Double readonly                = cppgc,
        attr          "m33"              : Double readonly                = cppgc,
        attr          "m34"              : Double readonly                = cppgc,
        attr          "m41"              : Double readonly                = cppgc,
        attr          "m42"              : Double readonly                = cppgc,
        attr          "m43"              : Double readonly                = cppgc,
        attr          "m44"              : Double readonly                = cppgc,
        attr          "is2D"             : Boolean readonly                = cppgc,
        attr          "isIdentity"       : Boolean readonly                = cppgc,
        method        "flipX"            : Any             len 0          = cppgc,
        method        "flipY"            : Any             len 0          = cppgc,
        method        "inverse"          : Any             len 0          = cppgc,
        method        "multiply"         : Any             len 0          = cppgc,
        method        "rotate"           : Any             len 0          = cppgc,
        method        "rotateAxisAngle"  : Any             len 0          = cppgc,
        method        "rotateFromVector" : Any             len 0          = cppgc,
        method        "scale"            : Any             len 0          = cppgc,
        method        "scale3d"          : Any             len 0          = cppgc,
        method        "scaleNonUniform"  : Any             len 0          = cppgc,
        method        "skewX"            : Any             len 0          = cppgc,
        method        "skewY"            : Any             len 0          = cppgc,
        method        "toFloat32Array"   : Any             len 0          = cppgc,
        method        "toFloat64Array"   : Any             len 0          = cppgc,
        method        "toJSON"           : Any             len 0          = cppgc,
        method        "transformPoint"   : Any             len 0          = cppgc,
        method        "translate"        : Any             len 0          = cppgc,
        method        "toString"         : Any             len 0          = cppgc,
        static_method "fromFloat32Array" : Any             len 1 required 1 = cppgc,
        static_method "fromFloat64Array" : Any             len 1 required 1 = cppgc,
        static_method "fromMatrix"       : Any             len 0          = cppgc,
    }
}

webapi::interface! {
    "DOMMatrix" : "DOMMatrixReadOnly" [value, both, class DOMMatrix] {
        ctor          "DOMMatrix"                     len 0            = stub,
        attr          "a"                   : Double                  = cppgc,
        attr          "b"                   : Double                  = cppgc,
        attr          "c"                   : Double                  = cppgc,
        attr          "d"                   : Double                  = cppgc,
        attr          "e"                   : Double                  = cppgc,
        attr          "f"                   : Double                  = cppgc,
        attr          "m11"                 : Double                  = cppgc,
        attr          "m12"                 : Double                  = cppgc,
        attr          "m13"                 : Double                  = cppgc,
        attr          "m14"                 : Double                  = cppgc,
        attr          "m21"                 : Double                  = cppgc,
        attr          "m22"                 : Double                  = cppgc,
        attr          "m23"                 : Double                  = cppgc,
        attr          "m24"                 : Double                  = cppgc,
        attr          "m31"                 : Double                  = cppgc,
        attr          "m32"                 : Double                  = cppgc,
        attr          "m33"                 : Double                  = cppgc,
        attr          "m34"                 : Double                  = cppgc,
        attr          "m41"                 : Double                  = cppgc,
        attr          "m42"                 : Double                  = cppgc,
        attr          "m43"                 : Double                  = cppgc,
        attr          "m44"                 : Double                  = cppgc,
        method        "invertSelf"           : Any        len 0        = cppgc,
        method        "multiplySelf"         : Any        len 0        = cppgc,
        method        "preMultiplySelf"      : Any        len 0        = cppgc,
        method        "rotateAxisAngleSelf"  : Any        len 0        = cppgc,
        method        "rotateFromVectorSelf" : Any        len 0        = cppgc,
        method        "rotateSelf"           : Any        len 0        = cppgc,
        method        "scale3dSelf"          : Any        len 0        = cppgc,
        method        "scaleSelf"            : Any        len 0        = cppgc,
        method        "skewXSelf"            : Any        len 0        = cppgc,
        method        "skewYSelf"            : Any        len 0        = cppgc,
        method        "translateSelf"        : Any        len 0        = cppgc,
        method        "setMatrixValue"       : Any        len 1 required 1 = cppgc,
        static_method "fromFloat32Array"     : Any        len 1 required 1 = cppgc,
        static_method "fromFloat64Array"     : Any        len 1 required 1 = cppgc,
        static_method "fromMatrix"           : Any        len 0        = cppgc,
    }
}
