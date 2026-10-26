//! The GLSL ES built-in function library.
//!
//! Every entry here is computed. A builtin that returned a plausible constant would be the same
//! failure as a `raster_gl` that ignores its command list, one level down: the shader would run, the
//! pixels would come back, and they would not be the page's.
//!
//! The one honest gap is the derivative family (`dFdx`/`dFdy`/`fwidth`, the
//! `OES_standard_derivatives` extension). Those are defined over a 2x2 quad of neighbouring
//! fragments, and this rasterizer shades one fragment at a time — so they return zero, which is what
//! the spec permits an implementation without derivatives to do and is why the extension is not in
//! the supported list the context reports.

use super::interp::{Interp, RunError, RunResult, Val};

/// Apply `f` component-wise to one argument, promoting to float.
fn map1(a: &Val, f: impl Fn(f32) -> f32) -> Val {
    Val::F(a.floats().iter().map(|&x| f(x)).collect())
}

/// Component-wise over two arguments, with scalar broadcast on either side.
fn map2(a: &Val, b: &Val, f: impl Fn(f32, f32) -> f32) -> Val {
    let (x, y) = (a.floats(), b.floats());
    let n = x.len().max(y.len());
    let pick = |v: &Vec<f32>, i: usize| if v.len() == 1 { v[0] } else { *v.get(i).unwrap_or(&0.0) };
    Val::F((0..n).map(|i| f(pick(&x, i), pick(&y, i))).collect())
}

fn map3(a: &Val, b: &Val, c: &Val, f: impl Fn(f32, f32, f32) -> f32) -> Val {
    let (x, y, z) = (a.floats(), b.floats(), c.floats());
    let n = x.len().max(y.len()).max(z.len());
    let pick = |v: &Vec<f32>, i: usize| if v.len() == 1 { v[0] } else { *v.get(i).unwrap_or(&0.0) };
    Val::F((0..n).map(|i| f(pick(&x, i), pick(&y, i), pick(&z, i))).collect())
}

fn cmp2(a: &Val, b: &Val, f: impl Fn(f32, f32) -> bool) -> Val {
    let (x, y) = (a.floats(), b.floats());
    let n = x.len().max(y.len());
    let pick = |v: &Vec<f32>, i: usize| if v.len() == 1 { v[0] } else { *v.get(i).unwrap_or(&0.0) };
    Val::B((0..n).map(|i| f(pick(&x, i), pick(&y, i))).collect())
}

fn dot(a: &Val, b: &Val) -> f32 {
    a.floats().iter().zip(b.floats().iter()).map(|(x, y)| x * y).sum()
}

fn length(a: &Val) -> f32 {
    a.floats().iter().map(|x| x * x).sum::<f32>().sqrt()
}

fn scale(a: &Val, k: f32) -> Val {
    Val::F(a.floats().iter().map(|x| x * k).collect())
}

fn sub(a: &Val, b: &Val) -> Val {
    map2(a, b, |x, y| x - y)
}

/// The zero of the same width as `a` — what `normalize` of a zero vector gives, and what several
/// geometric builtins fall back to rather than emitting NaN into the framebuffer.
fn zeros_like(a: &Val) -> Val {
    Val::F(vec![0.0; a.floats().len().max(1)])
}

pub fn call(it: &mut Interp, name: &str, a: &[Val]) -> RunResult<Val> {
    let arg = |i: usize| -> &Val { a.get(i).unwrap_or(&Val::Void) };
    Ok(match name {
        // --- angle and trigonometry ---
        "radians" => map1(arg(0), |x| x.to_radians()),
        "degrees" => map1(arg(0), |x| x.to_degrees()),
        "sin" => map1(arg(0), f32::sin),
        "cos" => map1(arg(0), f32::cos),
        "tan" => map1(arg(0), f32::tan),
        "asin" => map1(arg(0), |x| x.clamp(-1.0, 1.0).asin()),
        "acos" => map1(arg(0), |x| x.clamp(-1.0, 1.0).acos()),
        "atan" => {
            if a.len() >= 2 {
                map2(arg(0), arg(1), |y, x| y.atan2(x))
            } else {
                map1(arg(0), f32::atan)
            }
        }
        "sinh" => map1(arg(0), f32::sinh),
        "cosh" => map1(arg(0), f32::cosh),
        "tanh" => map1(arg(0), f32::tanh),
        "asinh" => map1(arg(0), f32::asinh),
        "acosh" => map1(arg(0), f32::acosh),
        "atanh" => map1(arg(0), f32::atanh),

        // --- exponential ---
        "pow" => map2(arg(0), arg(1), |x, y| x.powf(y)),
        "exp" => map1(arg(0), f32::exp),
        "log" => map1(arg(0), f32::ln),
        "exp2" => map1(arg(0), f32::exp2),
        "log2" => map1(arg(0), f32::log2),
        "sqrt" => map1(arg(0), |x| x.max(0.0).sqrt()),
        "inversesqrt" => map1(arg(0), |x| if x > 0.0 { 1.0 / x.sqrt() } else { 0.0 }),

        // --- common ---
        "abs" => match arg(0) {
            Val::I(v) => Val::I(v.iter().map(|x| x.abs()).collect()),
            other => map1(other, f32::abs),
        },
        "sign" => match arg(0) {
            Val::I(v) => Val::I(v.iter().map(|x| x.signum()).collect()),
            other => map1(other, |x| {
                if x > 0.0 {
                    1.0
                } else if x < 0.0 {
                    -1.0
                } else {
                    0.0
                }
            }),
        },
        "floor" => map1(arg(0), f32::floor),
        "ceil" => map1(arg(0), f32::ceil),
        "trunc" => map1(arg(0), f32::trunc),
        "round" => map1(arg(0), f32::round),
        "roundEven" => map1(arg(0), |x| {
            let r = x.round();
            if (x - x.trunc()).abs() == 0.5 && r % 2.0 != 0.0 {
                r - x.signum()
            } else {
                r
            }
        }),
        "fract" => map1(arg(0), |x| x - x.floor()),
        // GLSL `mod` is the floored modulus, NOT Rust's `%` (which truncates toward zero). The
        // difference shows on negative inputs, and negative inputs are exactly what a tiling shader
        // feeds it.
        "mod" => map2(arg(0), arg(1), |x, y| if y == 0.0 { 0.0 } else { x - y * (x / y).floor() }),
        "min" => match (arg(0), arg(1)) {
            (Val::I(x), Val::I(y)) => {
                let n = x.len().max(y.len());
                Val::I((0..n)
                    .map(|i| {
                        let p = if x.len() == 1 { x[0] } else { x[i] };
                        let q = if y.len() == 1 { y[0] } else { y[i] };
                        p.min(q)
                    })
                    .collect())
            }
            (p, q) => map2(p, q, f32::min),
        },
        "max" => match (arg(0), arg(1)) {
            (Val::I(x), Val::I(y)) => {
                let n = x.len().max(y.len());
                Val::I((0..n)
                    .map(|i| {
                        let p = if x.len() == 1 { x[0] } else { x[i] };
                        let q = if y.len() == 1 { y[0] } else { y[i] };
                        p.max(q)
                    })
                    .collect())
            }
            (p, q) => map2(p, q, f32::max),
        },
        "clamp" => match (arg(0), arg(1), arg(2)) {
            (Val::I(_), Val::I(_), Val::I(_)) => {
                let (x, lo, hi) = (arg(0).ints(), arg(1).ints(), arg(2).ints());
                Val::I((0..x.len())
                    .map(|i| {
                        let l = if lo.len() == 1 { lo[0] } else { lo[i] };
                        let h = if hi.len() == 1 { hi[0] } else { hi[i] };
                        x[i].clamp(l.min(h), h.max(l))
                    })
                    .collect())
            }
            _ => map3(arg(0), arg(1), arg(2), |x, lo, hi| if lo > hi { x } else { x.clamp(lo, hi) }),
        },
        // `mix(x, y, bvec)` selects; `mix(x, y, float)` interpolates.
        "mix" => match arg(2) {
            Val::B(sel) => {
                let (x, y) = (arg(0).floats(), arg(1).floats());
                Val::F((0..x.len().max(y.len()))
                    .map(|i| {
                        let s = if sel.len() == 1 { sel[0] } else { *sel.get(i).unwrap_or(&false) };
                        let px = if x.len() == 1 { x[0] } else { *x.get(i).unwrap_or(&0.0) };
                        let py = if y.len() == 1 { y[0] } else { *y.get(i).unwrap_or(&0.0) };
                        if s {
                            py
                        } else {
                            px
                        }
                    })
                    .collect())
            }
            _ => map3(arg(0), arg(1), arg(2), |x, y, t| x * (1.0 - t) + y * t),
        },
        "step" => map2(arg(0), arg(1), |edge, x| if x < edge { 0.0 } else { 1.0 }),
        "smoothstep" => map3(arg(0), arg(1), arg(2), |e0, e1, x| {
            if e0 == e1 {
                return if x < e0 { 0.0 } else { 1.0 };
            }
            let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        }),
        "isnan" => Val::B(arg(0).floats().iter().map(|x| x.is_nan()).collect()),
        "isinf" => Val::B(arg(0).floats().iter().map(|x| x.is_infinite()).collect()),

        // --- geometric ---
        "length" => Val::scalar_f(length(arg(0))),
        "distance" => Val::scalar_f(length(&sub(arg(0), arg(1)))),
        "dot" => Val::scalar_f(dot(arg(0), arg(1))),
        "cross" => {
            let (x, y) = (arg(0).floats(), arg(1).floats());
            if x.len() < 3 || y.len() < 3 {
                return Err(RunError::Unsupported("cross() needs two vec3".into()));
            }
            Val::F(vec![
                x[1] * y[2] - x[2] * y[1],
                x[2] * y[0] - x[0] * y[2],
                x[0] * y[1] - x[1] * y[0],
            ])
        }
        "normalize" => {
            let l = length(arg(0));
            if l == 0.0 {
                zeros_like(arg(0))
            } else {
                scale(arg(0), 1.0 / l)
            }
        }
        "faceforward" => {
            if dot(arg(2), arg(1)) < 0.0 {
                arg(0).clone()
            } else {
                scale(arg(0), -1.0)
            }
        }
        "reflect" => {
            let d = dot(arg(0), arg(1));
            sub(arg(0), &scale(arg(1), 2.0 * d))
        }
        "refract" => {
            let eta = arg(2).as_f();
            let d = dot(arg(1), arg(0));
            let k = 1.0 - eta * eta * (1.0 - d * d);
            if k < 0.0 {
                zeros_like(arg(0))
            } else {
                sub(&scale(arg(0), eta), &scale(arg(1), eta * d + k.sqrt()))
            }
        }

        // --- matrix ---
        "matrixCompMult" => match (arg(0), arg(1)) {
            (Val::M { cols, rows, v: x }, Val::M { v: y, .. }) => Val::M {
                cols: *cols,
                rows: *rows,
                v: x.iter().zip(y.iter()).map(|(p, q)| p * q).collect(),
            },
            _ => return Err(RunError::Unsupported("matrixCompMult() needs two matrices".into())),
        },
        "transpose" => match arg(0) {
            Val::M { cols, rows, v } => {
                let mut out = vec![0.0; cols * rows];
                for c in 0..*cols {
                    for r in 0..*rows {
                        out[r * cols + c] = v[c * rows + r];
                    }
                }
                Val::M { cols: *rows, rows: *cols, v: out }
            }
            _ => return Err(RunError::Unsupported("transpose() needs a matrix".into())),
        },
        "determinant" => match arg(0) {
            Val::M { cols, rows, v } if cols == rows => Val::scalar_f(det(v, *cols)),
            _ => return Err(RunError::Unsupported("determinant() needs a square matrix".into())),
        },
        "inverse" => match arg(0) {
            Val::M { cols, rows, v } if cols == rows => match invert(v, *cols) {
                Some(m) => Val::M { cols: *cols, rows: *rows, v: m },
                // GL leaves a singular inverse undefined; the identity is the least destructive
                // answer and keeps a NaN out of the framebuffer.
                None => {
                    let mut m = vec![0.0; cols * rows];
                    for i in 0..*cols {
                        m[i * rows + i] = 1.0;
                    }
                    Val::M { cols: *cols, rows: *rows, v: m }
                }
            },
            _ => return Err(RunError::Unsupported("inverse() needs a square matrix".into())),
        },
        "outerProduct" => {
            let (c, r) = (arg(1).floats(), arg(0).floats());
            let mut v = vec![0.0; c.len() * r.len()];
            for (ci, cv) in c.iter().enumerate() {
                for (ri, rv) in r.iter().enumerate() {
                    v[ci * r.len() + ri] = cv * rv;
                }
            }
            Val::M { cols: c.len(), rows: r.len(), v }
        }

        // --- vector relational ---
        "lessThan" => cmp2(arg(0), arg(1), |x, y| x < y),
        "lessThanEqual" => cmp2(arg(0), arg(1), |x, y| x <= y),
        "greaterThan" => cmp2(arg(0), arg(1), |x, y| x > y),
        "greaterThanEqual" => cmp2(arg(0), arg(1), |x, y| x >= y),
        "equal" => cmp2(arg(0), arg(1), |x, y| x == y),
        "notEqual" => cmp2(arg(0), arg(1), |x, y| x != y),
        "any" => Val::scalar_b(arg(0).bools().iter().any(|&b| b)),
        "all" => Val::scalar_b(arg(0).bools().iter().all(|&b| b)),
        "not" => Val::B(arg(0).bools().iter().map(|&b| !b).collect()),

        // --- texture lookup. `textureNNN` with a bias/lod argument samples the same level: this
        // rasterizer keeps one mip level, so the bias has nothing to select between. ---
        "texture2D" | "texture2DProj" | "texture2DLod" | "texture2DProjLod" | "texture" | "textureLod" => {
            let unit = arg(0).as_i();
            let c = arg(1).floats();
            let (s, t) = if name.contains("Proj") {
                let q = *c.last().unwrap_or(&1.0);
                let q = if q == 0.0 { 1.0 } else { q };
                (c.first().copied().unwrap_or(0.0) / q, c.get(1).copied().unwrap_or(0.0) / q)
            } else if c.len() >= 3 && name == "texture" {
                // `texture(samplerCube, vec3)` in ES3.
                let px = it.tex.sample_cube(unit, [c[0], c[1], c[2]]);
                return Ok(Val::F(px.to_vec()));
            } else {
                (c.first().copied().unwrap_or(0.0), c.get(1).copied().unwrap_or(0.0))
            };
            Val::F(it.tex.sample_2d(unit, s, t).to_vec())
        }
        "textureCube" | "textureCubeLod" => {
            let unit = arg(0).as_i();
            let c = arg(1).floats();
            let d = [
                c.first().copied().unwrap_or(0.0),
                c.get(1).copied().unwrap_or(0.0),
                c.get(2).copied().unwrap_or(0.0),
            ];
            Val::F(it.tex.sample_cube(unit, d).to_vec())
        }

        // --- derivatives: see the module comment. Zero, and the extension is not advertised. ---
        "dFdx" | "dFdy" => zeros_like(arg(0)),
        "fwidth" => zeros_like(arg(0)),

        // --- `arr.length()` reaches here with the array itself as the receiver ---
        "length_" => Val::scalar_i(arg(0).len() as i32),

        other => {
            return Err(RunError::Unsupported(format!("no matching function for call to '{other}'")))
        }
    })
}

fn det(v: &[f32], n: usize) -> f32 {
    match n {
        1 => v[0],
        2 => v[0] * v[3] - v[2] * v[1],
        _ => {
            // Expansion along the first column, which is exact for the 3x3/4x4 GLSL allows.
            let mut s = 0.0;
            for r in 0..n {
                let mut minor = Vec::with_capacity((n - 1) * (n - 1));
                for c in 1..n {
                    for rr in 0..n {
                        if rr != r {
                            minor.push(v[c * n + rr]);
                        }
                    }
                }
                let sign = if r % 2 == 0 { 1.0 } else { -1.0 };
                s += sign * v[r] * det(&minor, n - 1);
            }
            s
        }
    }
}

/// Gauss-Jordan, column-major in and out.
fn invert(v: &[f32], n: usize) -> Option<Vec<f32>> {
    let at = |m: &Vec<f32>, r: usize, c: usize| m[c * n + r];
    let mut a: Vec<f32> = v.to_vec();
    let mut inv = vec![0.0; n * n];
    for i in 0..n {
        inv[i * n + i] = 1.0;
    }
    for col in 0..n {
        let mut piv = col;
        for r in col..n {
            if at(&a, r, col).abs() > at(&a, piv, col).abs() {
                piv = r;
            }
        }
        if at(&a, piv, col).abs() < 1e-12 {
            return None;
        }
        if piv != col {
            for c in 0..n {
                a.swap(c * n + piv, c * n + col);
                inv.swap(c * n + piv, c * n + col);
            }
        }
        let d = at(&a, col, col);
        for c in 0..n {
            a[c * n + col] /= d;
            inv[c * n + col] /= d;
        }
        for r in 0..n {
            if r == col {
                continue;
            }
            let f = at(&a, r, col);
            if f == 0.0 {
                continue;
            }
            for c in 0..n {
                a[c * n + r] -= f * a[c * n + col];
                inv[c * n + r] -= f * inv[c * n + col];
            }
        }
    }
    Some(inv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gl::glsl::compile;
    use crate::gl::interp::NoTextures;
    use std::rc::Rc;

    fn eval(src: &str) -> Vec<f32> {
        let sh = Rc::new(compile(&format!("void main() {{ gl_FragColor = {src}; }}")).unwrap());
        let tex = NoTextures;
        let mut it = Interp::new(&sh, &tex, 1_000_000);
        it.init_globals().unwrap();
        it.run_main().unwrap();
        it.get("gl_FragColor").unwrap().floats()
    }

    #[test]
    fn mod_is_the_floored_modulus_on_negatives() {
        // Rust's `%` would give -1.0 here; GLSL's `mod` gives 2.0.
        assert_eq!(eval("vec4(mod(-1.0, 3.0))")[0], 2.0);
    }

    #[test]
    fn smoothstep_and_mix() {
        assert_eq!(eval("vec4(smoothstep(0.0, 1.0, 0.5))")[0], 0.5);
        assert_eq!(eval("vec4(mix(0.0, 10.0, 0.25))")[0], 2.5);
    }

    #[test]
    fn geometric_builtins() {
        assert_eq!(eval("vec4(length(vec3(3.0, 4.0, 0.0)))")[0], 5.0);
        assert_eq!(eval("vec4(dot(vec2(1.0, 2.0), vec2(3.0, 4.0)))")[0], 11.0);
        let c = eval("vec4(cross(vec3(1.0,0.0,0.0), vec3(0.0,1.0,0.0)), 1.0)");
        assert_eq!(&c[..3], &[0.0, 0.0, 1.0]);
    }

    #[test]
    fn matrix_inverse_round_trips() {
        let v = eval(
            "vec4(inverse(mat2(2.0, 0.0, 0.0, 4.0)) * mat2(2.0, 0.0, 0.0, 4.0) * vec2(1.0, 1.0), 0.0, 1.0)",
        );
        assert!((v[0] - 1.0).abs() < 1e-5 && (v[1] - 1.0).abs() < 1e-5, "{v:?}");
    }

    #[test]
    fn an_unknown_function_is_an_error_not_a_wrong_pixel() {
        let sh = Rc::new(compile("void main() { gl_FragColor = vec4(nosuchfn(1.0)); }").unwrap());
        let tex = NoTextures;
        let mut it = Interp::new(&sh, &tex, 1000);
        it.init_globals().unwrap();
        assert!(matches!(it.run_main(), Err(RunError::Unsupported(_))));
    }
}
