//! Chromium's `gfx::Transform` as Blink's LAYOUT composes a box's CSS transform and maps its corners
//! (`ui/gfx/geometry/transform.cc`, `matrix44.cc`, `axis_transform2d.h`, Chrome 150) -- the arithmetic, not just
//! the matrix.
//!
//! `getBoundingClientRect` of a transformed box is float32 data in Chrome: each corner is mapped through the
//! transform and rounded to a float, the bounding box is taken in float. WHICH float depends on how the transform
//! was built and applied: a `gfx::Transform` stays an `AxisTransform2d` -- a FLOAT scale and translation -- until an
//! operation that is not a 2D scale or translation turns it into a double `Matrix44`, every `Matrix44` step adds
//! its lanes in its own order, and a point mapped through a full matrix is divided by its w in double before it
//! becomes a float. The same transform built the textbook way (origin-relative corners through one double matrix)
//! read 1 to 10 float32 ULPs off Chrome on a bot challenge's ClientRects probe (`skewY`, `matrix()`,
//! `scale(1e35, 1.89)`, `perspective() translateZ()`). This file is those methods, line for line; the comments name
//! the Chromium method each one is.
//!
//! The matrix is column-major, `m[col][row]`, as `Matrix44::matrix_` is.

type M44 = [[f64; 4]; 4];

const IDENTITY: M44 = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];

/// `gfx::Transform`: an `AxisTransform2d` (float scale, then float translation) until an operation needs the
/// full double matrix (`full_matrix_`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Transform {
    full: bool,
    scale: [f32; 2],
    trans: [f32; 2],
    m: M44,
}

/// `ClampFloatGeometry(double)`: a value within +-`FLT_MAX / 1e6` as the nearest float, the bound beyond it, and 0
/// for NaN (`base::saturated_cast` with `FloatGeometrySaturationHandler`; the bound is computed in double, then
/// made a float, as the constexpr is).
pub(crate) fn clamp_float_geometry(v: f64) -> f32 {
    let max = (f32::MAX as f64 / 1e6) as f32;
    if v.is_nan() {
        0.0
    } else if v > max as f64 {
        max
    } else if v < -(max as f64) {
        -max
    } else {
        v as f32
    }
}

/// The float form, for the `AxisTransform2d` mapping (`ClampFloatGeometry(float)`).
fn clamp_float_geometry_f32(v: f32) -> f32 {
    let max = (f32::MAX as f64 / 1e6) as f32;
    if v.is_nan() {
        0.0
    } else {
        v.clamp(-max, max)
    }
}

/// `base::DegToRad`: `deg * pi / 180`.
fn deg_to_rad(deg: f64) -> f64 {
    deg * std::f64::consts::PI / 180.0
}

/// `TanDegrees`.
fn tan_degrees(deg: f64) -> f64 {
    deg_to_rad(deg).tan()
}

/// `SinCosDegrees` (`ui/gfx/geometry/sin_cos_degrees.h`): exact at multiples of 45 degrees, range-reduced to
/// [0, 45] otherwise.
fn sin_cos_degrees(mut degrees: f64) -> (f64, f64) {
    if degrees > -90000000.0 && degrees < 90000000.0 {
        let n45 = degrees / 45.0;
        let mut octant = n45 as i32;
        if octant as f64 == n45 {
            let h = std::f64::consts::SQRT_2 / 2.0;
            const T: [(f64, f64); 8] = [(0.0, 1.0), (0.0, 0.0), (1.0, 0.0), (0.0, 0.0), (0.0, -1.0), (0.0, 0.0), (-1.0, 0.0), (0.0, 0.0)];
            let i = (octant & 7) as usize;
            return match i {
                1 => (h, h),
                3 => (h, -h),
                5 => (-h, -h),
                7 => (-h, h),
                _ => T[i],
            };
        }
        if degrees < 0.0 {
            octant -= 1;
        }
        degrees -= octant as f64 * 45.0;
        if octant & 1 != 0 {
            degrees = 45.0 - degrees;
        }
        let rad = deg_to_rad(degrees);
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
    let rad = deg_to_rad(degrees % 360.0);
    (rad.sin(), rad.cos())
}

// ── Matrix44 (lane by lane, in each method's own order) ─────────────────────────────────────────────────────────

fn col_scale(c: [f64; 4], s: f64) -> [f64; 4] {
    [c[0] * s, c[1] * s, c[2] * s, c[3] * s]
}
fn col_add(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2], a[3] + b[3]]
}
fn col_sub(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2], a[3] - b[3]]
}

/// `Matrix44::IsFlat`.
fn is_flat(m: &M44) -> bool {
    m[2] == [0.0, 0.0, 1.0, 0.0] && m[0][2] == 0.0 && m[1][2] == 0.0 && m[3][2] == 0.0
}
/// `Matrix44::HasPerspective`.
fn has_perspective(m: &M44) -> bool {
    !(m[0][3] == 0.0 && m[1][3] == 0.0 && m[2][3] == 0.0 && m[3][3] == 1.0)
}
/// `Matrix44::Is2dTransform`.
fn is_2d(m: &M44) -> bool {
    is_flat(m) && !has_perspective(m)
}

/// `Matrix44::PreTranslate`: `col3 = col0 * dx + col1 * dy + col3`.
fn m_pre_translate(m: &mut M44, dx: f64, dy: f64) {
    m[3] = col_add(col_add(col_scale(m[0], dx), col_scale(m[1], dy)), m[3]);
}
/// `Matrix44::PreTranslate3d`.
fn m_pre_translate3d(m: &mut M44, dx: f64, dy: f64, dz: f64) {
    if dx == 0.0 && dy == 0.0 && dz == 0.0 {
        return;
    }
    m[3] = col_add(col_add(col_add(col_scale(m[0], dx), col_scale(m[1], dy)), col_scale(m[2], dz)), m[3]);
}
/// `Matrix44::PostTranslate`.
fn m_post_translate(m: &mut M44, dx: f64, dy: f64) {
    if !has_perspective(m) {
        m[3][0] += dx;
        m[3][1] += dy;
        return;
    }
    if dx != 0.0 {
        for c in m.iter_mut() {
            c[0] += c[3] * dx;
        }
    }
    if dy != 0.0 {
        for c in m.iter_mut() {
            c[1] += c[3] * dy;
        }
    }
}
/// `Matrix44::PreScale`.
fn m_pre_scale(m: &mut M44, sx: f64, sy: f64) {
    m[0] = col_scale(m[0], sx);
    m[1] = col_scale(m[1], sy);
}
/// `Matrix44::PreScale3d`.
fn m_pre_scale3d(m: &mut M44, sx: f64, sy: f64, sz: f64) {
    if sx == 1.0 && sy == 1.0 && sz == 1.0 {
        return;
    }
    m[0] = col_scale(m[0], sx);
    m[1] = col_scale(m[1], sy);
    m[2] = col_scale(m[2], sz);
}
/// `Matrix44::PostScale`.
fn m_post_scale(m: &mut M44, sx: f64, sy: f64) {
    if sx != 1.0 {
        for c in m.iter_mut() {
            c[0] *= sx;
        }
    }
    if sy != 1.0 {
        for c in m.iter_mut() {
            c[1] *= sy;
        }
    }
}
/// `Matrix44::Skew`.
fn m_skew(m: &mut M44, tan_x: f64, tan_y: f64) {
    let (c0, c1) = (m[0], m[1]);
    m[0] = col_add(c0, col_scale(c1, tan_y));
    m[1] = col_add(c1, col_scale(c0, tan_x));
}
/// `Matrix44::ApplyPerspectiveDepth`.
fn m_apply_perspective_depth(m: &mut M44, p: f64) {
    m[2] = col_add(m[2], col_scale(m[3], -1.0 / p));
}
/// `Matrix44::RotateAboutZAxisSinCos`.
fn m_rotate_z(m: &mut M44, s: f64, c: f64) {
    let (c0, c1) = (m[0], m[1]);
    m[0] = col_add(col_scale(c0, c), col_scale(c1, s));
    m[1] = col_sub(col_scale(c1, c), col_scale(c0, s));
}
/// `Matrix44::RotateAboutXAxisSinCos`.
fn m_rotate_x(m: &mut M44, s: f64, c: f64) {
    let (c1, c2) = (m[1], m[2]);
    m[1] = col_add(col_scale(c1, c), col_scale(c2, s));
    m[2] = col_sub(col_scale(c2, c), col_scale(c1, s));
}
/// `Matrix44::RotateAboutYAxisSinCos`.
fn m_rotate_y(m: &mut M44, s: f64, c: f64) {
    let (c0, c2) = (m[0], m[2]);
    m[0] = col_sub(col_scale(c0, c), col_scale(c2, s));
    m[2] = col_add(col_scale(c2, c), col_scale(c0, s));
}
/// `Matrix44::SetConcat(x, y)`: `x * y`, the 2D fast path when both are 2D.
fn m_concat(x: &M44, y: &M44) -> M44 {
    if is_2d(x) && is_2d(y) {
        let (a, b, c, d, e, f) = (x[0][0], x[0][1], x[1][0], x[1][1], x[3][0], x[3][1]);
        let (ya, yb, yc, yd, ye, yf) = (y[0][0], y[0][1], y[1][0], y[1][1], y[3][0], y[3][1]);
        return [
            [a * ya + c * yb, b * ya + d * yb, 0.0, 0.0],
            [a * yc + c * yd, b * yc + d * yd, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [a * ye + c * yf + e, b * ye + d * yf + f, 0.0, 1.0],
        ];
    }
    let mut out = [[0.0; 4]; 4];
    for (i, mc) in y.iter().enumerate() {
        out[i] = col_add(col_add(col_add(col_scale(x[0], mc[0]), col_scale(x[1], mc[1])), col_scale(x[2], mc[2])), col_scale(x[3], mc[3]));
    }
    out
}
/// `Matrix44::RotateUnitSinCos`.
fn m_rotate_unit(m: &mut M44, x: f64, y: f64, z: f64, s: f64, c: f64) {
    if z == 1.0 {
        return m_rotate_z(m, s, c);
    }
    if y == 1.0 {
        return m_rotate_y(m, s, c);
    }
    if x == 1.0 {
        return m_rotate_x(m, s, c);
    }
    let cc = 1.0 - c;
    let (xs, ys, zs) = (x * s, y * s, z * s);
    let (xc, yc, zc) = (x * cc, y * cc, z * cc);
    let (xyc, yzc, zxc) = (x * yc, y * zc, z * xc);
    let r: M44 = [
        [x * xc + c, xyc + zs, zxc - ys, 0.0],
        [xyc - zs, y * yc + c, yzc + xs, 0.0],
        [zxc + ys, yzc - xs, z * zc + c, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    *m = m_concat(m, &r);
}

impl Transform {
    pub(crate) fn identity() -> Self {
        Transform { full: false, scale: [1.0, 1.0], trans: [0.0, 0.0], m: IDENTITY }
    }

    /// `Transform::ColMajor` (a full matrix).
    pub(crate) fn col_major(a: [f64; 16]) -> Self {
        let m = [[a[0], a[1], a[2], a[3]], [a[4], a[5], a[6], a[7]], [a[8], a[9], a[10], a[11]], [a[12], a[13], a[14], a[15]]];
        Transform { full: true, scale: [1.0, 1.0], trans: [0.0, 0.0], m }
    }

    /// `Transform::Affine` -- always a full double matrix.
    pub(crate) fn affine(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Self {
        Self::col_major([a, b, 0.0, 0.0, c, d, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, e, f, 0.0, 1.0])
    }

    /// `Transform::EnsureFullMatrix` (`AxisTransform2dToMatrix44`).
    fn ensure_full(&mut self) -> &mut M44 {
        if !self.full {
            self.full = true;
            self.m = [
                [self.scale[0] as f64, 0.0, 0.0, 0.0],
                [0.0, self.scale[1] as f64, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [self.trans[0] as f64, self.trans[1] as f64, 0.0, 1.0],
            ];
        }
        &mut self.m
    }

    /// `Transform::Translate(float, float)` (`AxisTransform2d::PreTranslate`: `translation += t * scale`, in float).
    pub(crate) fn translate(&mut self, x: f32, y: f32) {
        if !self.full {
            self.trans[0] += x * self.scale[0];
            self.trans[1] += y * self.scale[1];
        } else {
            m_pre_translate(&mut self.m, x as f64, y as f64);
        }
    }

    /// `Transform::Translate3d(float, float, float)`.
    pub(crate) fn translate3d(&mut self, x: f32, y: f32, z: f32) {
        if z == 0.0 {
            self.translate(x, y);
        } else {
            m_pre_translate3d(self.ensure_full(), x as f64, y as f64, z as f64);
        }
    }

    /// `Transform::PostTranslate(float, float)`.
    pub(crate) fn post_translate(&mut self, x: f32, y: f32) {
        if !self.full {
            self.trans[0] += x;
            self.trans[1] += y;
        } else {
            m_post_translate(&mut self.m, x as f64, y as f64);
        }
    }

    /// `Transform::Scale(float, float)`.
    pub(crate) fn scale(&mut self, x: f32, y: f32) {
        if !self.full {
            self.scale[0] *= x;
            self.scale[1] *= y;
        } else {
            m_pre_scale(&mut self.m, x as f64, y as f64);
        }
    }

    /// `Transform::PostScale(float, float)` (`AxisTransform2d::PostScale` scales the translation too).
    fn post_scale(&mut self, x: f32, y: f32) {
        if !self.full {
            self.scale[0] *= x;
            self.scale[1] *= y;
            self.trans[0] *= x;
            self.trans[1] *= y;
        } else {
            m_post_scale(&mut self.m, x as f64, y as f64);
        }
    }

    /// `Transform::Scale3d(float, float, float)`.
    pub(crate) fn scale3d(&mut self, x: f32, y: f32, z: f32) {
        if z == 1.0 {
            self.scale(x, y);
        } else {
            m_pre_scale3d(self.ensure_full(), x as f64, y as f64, z as f64);
        }
    }

    /// `Transform::Skew(double, double)` (degrees).
    pub(crate) fn skew(&mut self, degrees_x: f64, degrees_y: f64) {
        if degrees_x == 0.0 && degrees_y == 0.0 {
            return;
        }
        let (tx, ty) = (tan_degrees(degrees_x), tan_degrees(degrees_y));
        m_skew(self.ensure_full(), tx, ty);
    }

    /// `Transform::ApplyPerspectiveDepth`.
    pub(crate) fn apply_perspective_depth(&mut self, depth: f64) {
        if depth == 0.0 {
            return;
        }
        m_apply_perspective_depth(self.ensure_full(), depth);
    }

    /// `Transform::RotateAboutZAxis` (`rotate()`).
    pub(crate) fn rotate_about_z(&mut self, degrees: f64) {
        let (s, c) = sin_cos_degrees(degrees);
        if s == 0.0 && c == 1.0 {
            return;
        }
        m_rotate_z(self.ensure_full(), s, c);
    }

    /// `Transform::RotateAbout(x, y, z, degrees)` (`rotate3d()`, `rotateX/Y/Z()`).
    pub(crate) fn rotate_about(&mut self, mut x: f64, mut y: f64, mut z: f64, degrees: f64) {
        let (s, c) = sin_cos_degrees(degrees);
        if s == 0.0 && c == 1.0 {
            return;
        }
        let square_length = x * x + y * y + z * z;
        if square_length == 0.0 {
            return;
        }
        if square_length != 1.0 {
            let scale = 1.0 / square_length.sqrt();
            x *= scale;
            y *= scale;
            z *= scale;
        }
        m_rotate_unit(self.ensure_full(), x, y, z, s, c);
    }

    /// `Transform::PreConcat`: `this = this * t`.
    pub(crate) fn pre_concat(&mut self, t: &Transform) {
        if !t.full {
            // `PreConcat(const AxisTransform2d&)`: Translate, then Scale
            self.translate(t.trans[0], t.trans[1]);
            self.scale(t.scale[0], t.scale[1]);
        } else if !self.full {
            // the axis part goes after the full matrix: `PostConcat(self)` = PostScale, then PostTranslate
            let (s, tr) = (self.scale, self.trans);
            *self = *t;
            self.post_scale(s[0], s[1]);
            self.post_translate(tr[0], tr[1]);
        } else {
            self.m = m_concat(&self.m, &t.m);
        }
    }

    /// `Transform::Flatten` (`Matrix44::Flatten`: the z column and row made identity) -- nothing to an axis transform.
    pub(crate) fn flatten(&mut self) {
        if self.full {
            let m = &mut self.m;
            m[0][2] = 0.0;
            m[1][2] = 0.0;
            m[3][2] = 0.0;
            m[2] = [0.0, 0.0, 1.0, 0.0];
        }
    }

    /// `t * this` (`gfx::Transform::operator*`: `t` pre-concatenated with this).
    pub(crate) fn concat_after(&mut self, t: &Transform) {
        let mut r = *t;
        r.pre_concat(self);
        *self = r;
    }

    /// `Transform::MapPoint(PointF)`: in float through an `AxisTransform2d` (`x * scale + translation`), through
    /// a full matrix in double (`Matrix44::MapVector2` -- the point's z is 0 -- then divided by w when w is a
    /// normal number other than 1), clamped to a float.
    pub(crate) fn map_point(&self, x: f32, y: f32) -> (f32, f32) {
        if !self.full {
            return (clamp_float_geometry_f32(x * self.scale[0] + self.trans[0]), clamp_float_geometry_f32(y * self.scale[1] + self.trans[1]));
        }
        let m = &self.m;
        let (v0, v1) = (x as f64, y as f64);
        let rx = v0 * m[0][0] + v1 * m[1][0] + m[3][0];
        let ry = v0 * m[0][1] + v1 * m[1][1] + m[3][1];
        let w = v0 * m[0][3] + v1 * m[1][3] + m[3][3];
        if w != 1.0 && w.is_normal() {
            let wi = 1.0 / w;
            (clamp_float_geometry(rx * wi), clamp_float_geometry(ry * wi))
        } else {
            (clamp_float_geometry(rx), clamp_float_geometry(ry))
        }
    }
}

/// One CSS transform function as Blink's style builder stores it and its `TransformOperation::Apply` applies it.
#[derive(Clone, Debug)]
pub(crate) enum Op {
    /// `translate*()`: `Translate3d(float, float, float)`.
    Translate(f32, f32, f32),
    /// `scale*()`: `Scale3d(float, float, float)` (the stored doubles passed as floats).
    Scale(f32, f32, f32),
    /// `skew*()`, degrees.
    Skew(f64, f64),
    /// `rotate()` -- `Transform::Rotate`, about z.
    Rotate(f64),
    /// `rotate3d()` / `rotateX/Y/Z()` -- `RotateAbout(axis, angle)`.
    RotateAbout(f64, f64, f64, f64),
    /// `perspective(d)` -- `ApplyPerspectiveDepth(max(1, d))`; `perspective(none)` applies nothing.
    Perspective(Option<f64>),
    /// `matrix()` / `matrix3d()` -- `PreConcat` of the full matrix.
    Matrix(Transform),
}

impl Op {
    fn apply(&self, t: &mut Transform) {
        match *self {
            Op::Translate(x, y, z) => t.translate3d(x, y, z),
            Op::Scale(x, y, z) => t.scale3d(x, y, z),
            Op::Skew(ax, ay) => t.skew(ax, ay),
            Op::Rotate(a) => t.rotate_about_z(a),
            Op::RotateAbout(x, y, z, a) => t.rotate_about(x, y, z, a),
            Op::Perspective(Some(d)) => t.apply_perspective_depth(d.max(1.0)),
            Op::Perspective(None) => {}
            Op::Matrix(ref m) => t.pre_concat(m),
        }
    }
}

/// `ComputedStyle::ApplyTransform` with the transform origin: `Translate3d(origin)`, every operation, then
/// `Translate3d(-origin)`.
pub(crate) fn layer_transform(ops: &[Op], origin: (f32, f32, f32)) -> Transform {
    let mut t = Transform::identity();
    t.translate3d(origin.0, origin.1, origin.2);
    for op in ops {
        op.apply(&mut t);
    }
    t.translate3d(-origin.0, -origin.1, -origin.2);
    t
}

/// A computed `transform` value's functions as [`Op`]s -- `None` for one this file does not model, so the caller
/// keeps its own mapping rather than guessing. Lengths are px (the computed value's); angles any CSS unit.
pub(crate) fn parse_ops(value: &str) -> Option<Vec<Op>> {
    let mut ops = Vec::new();
    let mut rest = value.trim();
    while !rest.is_empty() {
        let open = rest.find('(')?;
        let name = rest[..open].trim().to_ascii_lowercase();
        let close = rest[open..].find(')')? + open;
        let args: Vec<&str> = rest[open + 1..close].split(|c: char| c == ',' || c.is_whitespace()).filter(|s| !s.is_empty()).collect();
        rest = rest[close + 1..].trim_start();
        let num = |s: &str| -> Option<f64> { s.trim().parse::<f64>().ok() };
        let px = |s: &str| -> Option<f64> {
            let s = s.trim();
            if s == "0" {
                return Some(0.0);
            }
            s.strip_suffix("px").and_then(|n| n.parse::<f64>().ok())
        };
        let deg = |s: &str| -> Option<f64> {
            let s = s.trim().to_ascii_lowercase();
            if s == "0" {
                return Some(0.0);
            }
            if let Some(n) = s.strip_suffix("deg") {
                return n.parse::<f64>().ok();
            }
            if let Some(n) = s.strip_suffix("grad") {
                return n.parse::<f64>().ok().map(|g| g * 360.0 / 400.0);
            }
            if let Some(n) = s.strip_suffix("rad") {
                return n.parse::<f64>().ok().map(|r| r * 180.0 / std::f64::consts::PI);
            }
            if let Some(n) = s.strip_suffix("turn") {
                return n.parse::<f64>().ok().map(|t| t * 360.0);
            }
            None
        };
        let a = |i: usize| args.get(i).copied();
        let op = match name.as_str() {
            "matrix" if args.len() == 6 => {
                let v: Vec<f64> = args.iter().map(|s| num(s)).collect::<Option<_>>()?;
                Op::Matrix(Transform::affine(v[0], v[1], v[2], v[3], v[4], v[5]))
            }
            "matrix3d" if args.len() == 16 => {
                let v: Vec<f64> = args.iter().map(|s| num(s)).collect::<Option<_>>()?;
                let mut a16 = [0.0; 16];
                a16.copy_from_slice(&v);
                Op::Matrix(Transform::col_major(a16))
            }
            "translate" if !args.is_empty() && args.len() <= 2 => Op::Translate(px(a(0)?)? as f32, a(1).map_or(Some(0.0), px)? as f32, 0.0),
            "translatex" if args.len() == 1 => Op::Translate(px(a(0)?)? as f32, 0.0, 0.0),
            "translatey" if args.len() == 1 => Op::Translate(0.0, px(a(0)?)? as f32, 0.0),
            "translatez" if args.len() == 1 => Op::Translate(0.0, 0.0, px(a(0)?)? as f32),
            "translate3d" if args.len() == 3 => Op::Translate(px(a(0)?)? as f32, px(a(1)?)? as f32, px(a(2)?)? as f32),
            "scale" if !args.is_empty() && args.len() <= 2 => {
                let x = num(a(0)?)?;
                Op::Scale(x as f32, a(1).map_or(Some(x), num)? as f32, 1.0)
            }
            "scalex" if args.len() == 1 => Op::Scale(num(a(0)?)? as f32, 1.0, 1.0),
            "scaley" if args.len() == 1 => Op::Scale(1.0, num(a(0)?)? as f32, 1.0),
            "scalez" if args.len() == 1 => Op::Scale(1.0, 1.0, num(a(0)?)? as f32),
            "scale3d" if args.len() == 3 => Op::Scale(num(a(0)?)? as f32, num(a(1)?)? as f32, num(a(2)?)? as f32),
            "skew" if !args.is_empty() && args.len() <= 2 => Op::Skew(deg(a(0)?)?, a(1).map_or(Some(0.0), deg)?),
            "skewx" if args.len() == 1 => Op::Skew(deg(a(0)?)?, 0.0),
            "skewy" if args.len() == 1 => Op::Skew(0.0, deg(a(0)?)?),
            "rotate" if args.len() == 1 => Op::Rotate(deg(a(0)?)?),
            "rotatez" if args.len() == 1 => Op::RotateAbout(0.0, 0.0, 1.0, deg(a(0)?)?),
            "rotatex" if args.len() == 1 => Op::RotateAbout(1.0, 0.0, 0.0, deg(a(0)?)?),
            "rotatey" if args.len() == 1 => Op::RotateAbout(0.0, 1.0, 0.0, deg(a(0)?)?),
            "rotate3d" if args.len() == 4 => Op::RotateAbout(num(a(0)?)?, num(a(1)?)?, num(a(2)?)?, deg(a(3)?)?),
            "perspective" if args.len() == 1 => {
                if a(0)?.eq_ignore_ascii_case("none") {
                    Op::Perspective(None)
                } else {
                    Op::Perspective(Some(px(a(0)?)?))
                }
            }
            _ => return None,
        };
        ops.push(op);
    }
    Some(ops)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_axis_transform_maps_in_float_and_a_full_one_in_double() {
        // translate + scale stays an AxisTransform2d: x * 1.89f + 7f, all float
        let mut t = Transform::identity();
        t.translate3d(7.0, 0.0, 0.0);
        t.scale3d(1.0, 1.89, 1.0);
        assert!(!t.full);
        assert_eq!(t.map_point(0.0, 44.0).1, 44.0f32 * 1.89f32);
        // a skew makes it full
        t.skew(0.0, 23.1753218);
        assert!(t.full);
    }

    #[test]
    fn rotations_at_multiples_of_45_degrees_are_exact() {
        assert_eq!(sin_cos_degrees(90.0), (1.0, 0.0));
        assert_eq!(sin_cos_degrees(-90.0), (-1.0, 0.0));
        assert_eq!(sin_cos_degrees(180.0), (0.0, -1.0));
    }

    #[test]
    fn the_parser_takes_every_function_or_none() {
        assert_eq!(parse_ops("perspective(12890px) translateZ(101.5px)").map(|o| o.len()), Some(2));
        assert_eq!(parse_ops("matrix(0.95559, 2.13329, -0.9842, 0.98423, 150, 95)").map(|o| o.len()), Some(1));
        assert!(parse_ops("translate(10%, 5px)").is_none());
    }
}
