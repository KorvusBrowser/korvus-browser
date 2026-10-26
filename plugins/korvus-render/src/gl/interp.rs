//! The GLSL ES interpreter — the thing that makes a WebGL frame depend on the page's program.
//!
//! It tree-walks the AST `glsl.rs` produced, once per vertex and once per fragment. That is slow
//! compared with a JIT and irrelevant at the sizes this runs at: a fingerprint probe is a handful of
//! triangles on a 256x128 canvas, and the alternative on this box is not a faster renderer but no
//! renderer at all.
//!
//! Every shader run carries a **fuel budget**. A page controls this program completely, and `for (int
//! i = 0; i < 1000000; i++)` in a fragment shader is a page-authored infinite loop inside a
//! synchronous `toDataURL()`. Running out of fuel aborts the draw and is reported as a GL error, the
//! same shape as a real driver's context loss under a hung shader.

use std::collections::HashMap;
use std::rc::Rc;

use super::glsl::{BinOp, Expr, Func, Qual, Shader, Stmt, Type, UnOp};

/// A runtime value. Numeric types are flat component vectors; `M` is column-major, like GL.
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    Void,
    B(Vec<bool>),
    I(Vec<i32>),
    U(Vec<u32>),
    F(Vec<f32>),
    M { cols: usize, rows: usize, v: Vec<f32> },
    Arr(Vec<Val>),
    Struct(Vec<(String, Val)>),
}

impl Val {
    pub fn scalar_f(v: f32) -> Val {
        Val::F(vec![v])
    }
    pub fn scalar_i(v: i32) -> Val {
        Val::I(vec![v])
    }
    pub fn scalar_b(v: bool) -> Val {
        Val::B(vec![v])
    }

    /// The components as floats — the form arithmetic works in. `bool` is 1.0/0.0, as GLSL's own
    /// `float(bvec)` constructor gives.
    pub fn floats(&self) -> Vec<f32> {
        match self {
            Val::F(v) => v.clone(),
            Val::I(v) => v.iter().map(|&x| x as f32).collect(),
            Val::U(v) => v.iter().map(|&x| x as f32).collect(),
            Val::B(v) => v.iter().map(|&x| if x { 1.0 } else { 0.0 }).collect(),
            Val::M { v, .. } => v.clone(),
            _ => Vec::new(),
        }
    }

    pub fn ints(&self) -> Vec<i32> {
        match self {
            Val::I(v) => v.clone(),
            Val::U(v) => v.iter().map(|&x| x as i32).collect(),
            Val::F(v) => v.iter().map(|&x| x as i32).collect(),
            Val::B(v) => v.iter().map(|&x| x as i32).collect(),
            _ => Vec::new(),
        }
    }

    pub fn bools(&self) -> Vec<bool> {
        match self {
            Val::B(v) => v.clone(),
            Val::I(v) => v.iter().map(|&x| x != 0).collect(),
            Val::U(v) => v.iter().map(|&x| x != 0).collect(),
            Val::F(v) => v.iter().map(|&x| x != 0.0).collect(),
            _ => Vec::new(),
        }
    }

    pub fn as_f(&self) -> f32 {
        self.floats().first().copied().unwrap_or(0.0)
    }
    pub fn as_i(&self) -> i32 {
        self.ints().first().copied().unwrap_or(0)
    }
    pub fn as_bool(&self) -> bool {
        self.bools().first().copied().unwrap_or(false)
    }

    pub fn len(&self) -> usize {
        match self {
            Val::B(v) => v.len(),
            Val::I(v) => v.len(),
            Val::U(v) => v.len(),
            Val::F(v) => v.len(),
            Val::M { v, .. } => v.len(),
            Val::Arr(v) => v.len(),
            Val::Struct(v) => v.len(),
            Val::Void => 0,
        }
    }

    /// A vec4 read of a value of any width — the shape `gl_Position` and `gl_FragColor` need.
    pub fn vec4(&self) -> [f32; 4] {
        let f = self.floats();
        [
            f.first().copied().unwrap_or(0.0),
            f.get(1).copied().unwrap_or(0.0),
            f.get(2).copied().unwrap_or(0.0),
            f.get(3).copied().unwrap_or(if f.len() == 3 { 1.0 } else { 0.0 }),
        ]
    }
}

/// Where `texture2D` gets its texels. Implemented by the GL state machine, which owns the texture
/// units and their filter/wrap state.
pub trait TextureSource {
    fn sample_2d(&self, unit: i32, s: f32, t: f32) -> [f32; 4];
    fn sample_cube(&self, unit: i32, d: [f32; 3]) -> [f32; 4];
}

/// A [`TextureSource`] with no textures — every sample is opaque black, which is what GL gives for
/// an incomplete texture.
pub struct NoTextures;
impl TextureSource for NoTextures {
    fn sample_2d(&self, _unit: i32, _s: f32, _t: f32) -> [f32; 4] {
        [0.0, 0.0, 0.0, 1.0]
    }
    fn sample_cube(&self, _unit: i32, _d: [f32; 3]) -> [f32; 4] {
        [0.0, 0.0, 0.0, 1.0]
    }
}

enum Flow {
    Normal,
    Break,
    Continue,
    Return(Val),
    Discard,
}

/// One shader invocation.
pub struct Interp<'a> {
    pub shader: &'a Rc<Shader>,
    /// Uniforms, attributes and inbound varyings — everything declared at file scope.
    pub globals: HashMap<String, Val>,
    scopes: Vec<HashMap<String, Val>>,
    pub tex: &'a dyn TextureSource,
    fuel: i64,
    /// Set when a fragment shader executed `discard`.
    pub discarded: bool,
}

/// What went wrong at runtime. Both cases end the draw call rather than the frame.
#[derive(Debug, Clone, PartialEq)]
pub enum RunError {
    /// The shader used more steps than the budget allows (a page-authored hang).
    OutOfFuel,
    /// Something the interpreter does not implement, named so it reaches `getError`/the info log
    /// rather than silently producing a wrong pixel.
    Unsupported(String),
}

pub type RunResult<T> = Result<T, RunError>;

impl<'a> Interp<'a> {
    pub fn new(shader: &'a Rc<Shader>, tex: &'a dyn TextureSource, fuel: i64) -> Self {
        Self { shader, globals: HashMap::new(), scopes: Vec::new(), tex, fuel, discarded: false }
    }

    pub fn set(&mut self, name: &str, v: Val) {
        self.globals.insert(name.to_string(), v);
    }
    pub fn get(&self, name: &str) -> Option<&Val> {
        for s in self.scopes.iter().rev() {
            if let Some(v) = s.get(name) {
                return Some(v);
            }
        }
        self.globals.get(name)
    }

    fn burn(&mut self, n: i64) -> RunResult<()> {
        self.fuel -= n;
        if self.fuel < 0 {
            return Err(RunError::OutOfFuel);
        }
        Ok(())
    }

    pub fn fuel_left(&self) -> i64 {
        self.fuel
    }

    /// Seed the globals with each declaration's default (or its constant initializer) so a shader
    /// that reads an unset varying gets zeroes rather than a missing-name error.
    pub fn init_globals(&mut self) -> RunResult<()> {
        let globals: Vec<_> = self.shader.globals.clone();
        for d in &globals {
            if self.globals.contains_key(&d.name) {
                continue;
            }
            let v = match &d.init {
                Some(e) => self.eval(e)?,
                None => self.zero(&d.ty),
            };
            self.globals.insert(d.name.clone(), v);
        }
        Ok(())
    }

    /// The zero value of a type — GLSL does not define one for locals, but a deterministic zero is
    /// the only honest choice for an uninitialized read, and it is what every driver produces in
    /// practice for the varyings a stage did not write.
    pub fn zero(&self, t: &Type) -> Val {
        match t {
            Type::Void => Val::Void,
            Type::Bool => Val::B(vec![false]),
            Type::Int | Type::Sampler2D | Type::SamplerCube => Val::I(vec![0]),
            Type::UInt => Val::U(vec![0]),
            Type::Float => Val::F(vec![0.0]),
            Type::Vec(n) => Val::F(vec![0.0; *n]),
            Type::IVec(n) => Val::I(vec![0; *n]),
            Type::UVec(n) => Val::U(vec![0; *n]),
            Type::BVec(n) => Val::B(vec![false; *n]),
            Type::Mat(c, r) => {
                let mut v = vec![0.0; c * r];
                for i in 0..(*c).min(*r) {
                    v[i * r + i] = 1.0;
                }
                Val::M { cols: *c, rows: *r, v }
            }
            Type::Struct(name) => {
                let fields = self.shader.structs.get(name).cloned().unwrap_or_default();
                Val::Struct(fields.iter().map(|(ft, fname)| (fname.clone(), self.zero(ft))).collect())
            }
            Type::Array(inner, n) => Val::Arr((0..*n).map(|_| self.zero(inner)).collect()),
        }
    }

    /// Run `main()`.
    pub fn run_main(&mut self) -> RunResult<()> {
        let f = match self.shader.func("main", 0) {
            Some(f) => f.clone(),
            None => return Err(RunError::Unsupported("shader has no main()".into())),
        };
        self.scopes.push(HashMap::new());
        let flow = self.exec_block(&f.body)?;
        self.scopes.pop();
        if matches!(flow, Flow::Discard) {
            self.discarded = true;
        }
        Ok(())
    }

    // ── statements ────────────────────────────────────────────────────────────────────────────

    fn exec_block(&mut self, stmts: &[Stmt]) -> RunResult<Flow> {
        for s in stmts {
            match self.exec(s)? {
                Flow::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal)
    }

    fn exec(&mut self, s: &Stmt) -> RunResult<Flow> {
        self.burn(1)?;
        match s {
            Stmt::Empty => Ok(Flow::Normal),
            Stmt::Expr(e) => {
                self.eval(e)?;
                Ok(Flow::Normal)
            }
            Stmt::Decls(ds) => {
                for d in ds {
                    let v = match &d.init {
                        Some(e) => {
                            let raw = self.eval(e)?;
                            coerce(&raw, &d.ty)
                        }
                        None => self.zero(&d.ty),
                    };
                    match self.scopes.last_mut() {
                        Some(sc) => {
                            sc.insert(d.name.clone(), v);
                        }
                        None => {
                            self.globals.insert(d.name.clone(), v);
                        }
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::Block(b) => {
                self.scopes.push(HashMap::new());
                let f = self.exec_block(b);
                self.scopes.pop();
                f
            }
            Stmt::If(c, then, els) => {
                if self.eval(c)?.as_bool() {
                    self.exec(then)
                } else if let Some(e) = els {
                    self.exec(e)
                } else {
                    Ok(Flow::Normal)
                }
            }
            Stmt::While(c, body) => {
                loop {
                    self.burn(1)?;
                    if !self.eval(c)?.as_bool() {
                        return Ok(Flow::Normal);
                    }
                    match self.exec(body)? {
                        Flow::Break => return Ok(Flow::Normal),
                        Flow::Normal | Flow::Continue => {}
                        other => return Ok(other),
                    }
                }
            }
            Stmt::DoWhile(body, c) => loop {
                self.burn(1)?;
                match self.exec(body)? {
                    Flow::Break => return Ok(Flow::Normal),
                    Flow::Normal | Flow::Continue => {}
                    other => return Ok(other),
                }
                if !self.eval(c)?.as_bool() {
                    return Ok(Flow::Normal);
                }
            },
            Stmt::For { init, cond, step, body } => {
                self.scopes.push(HashMap::new());
                let r = (|| -> RunResult<Flow> {
                    if let Some(i) = init {
                        self.exec(i)?;
                    }
                    loop {
                        self.burn(1)?;
                        if let Some(c) = cond {
                            if !self.eval(c)?.as_bool() {
                                return Ok(Flow::Normal);
                            }
                        }
                        match self.exec(body)? {
                            Flow::Break => return Ok(Flow::Normal),
                            Flow::Normal | Flow::Continue => {}
                            other => return Ok(other),
                        }
                        if let Some(s) = step {
                            self.eval(s)?;
                        }
                    }
                })();
                self.scopes.pop();
                r
            }
            Stmt::Return(v) => {
                let val = match v {
                    Some(e) => self.eval(e)?,
                    None => Val::Void,
                };
                Ok(Flow::Return(val))
            }
            Stmt::Switch(sel, arms) => {
                let v = self.eval(sel)?.ints().first().copied().unwrap_or(0);
                // The first `case` whose label equals the selector, else `default`; nothing runs when
                // neither exists.
                let mut start = None;
                for (i, (label, _)) in arms.iter().enumerate() {
                    if let Some(l) = label {
                        if self.eval(l)?.ints().first().copied() == Some(v) {
                            start = Some(i);
                            break;
                        }
                    }
                }
                let Some(start) = start.or_else(|| arms.iter().position(|(l, _)| l.is_none())) else {
                    return Ok(Flow::Normal);
                };
                // One scope for the whole body, as GLSL's switch body is one compound statement; arms
                // fall through until a `break`, which the switch consumes -- `continue`, `return` and
                // `discard` belong to what encloses it.
                self.scopes.push(HashMap::new());
                let run = (|| -> RunResult<Flow> {
                    for (_, body) in &arms[start..] {
                        for st in body {
                            match self.exec(st)? {
                                Flow::Normal => {}
                                Flow::Break => return Ok(Flow::Normal),
                                other => return Ok(other),
                            }
                        }
                    }
                    Ok(Flow::Normal)
                })();
                self.scopes.pop();
                run
            }
            Stmt::Break => Ok(Flow::Break),
            Stmt::Continue => Ok(Flow::Continue),
            Stmt::Discard => Ok(Flow::Discard),
        }
    }

    // ── expressions ───────────────────────────────────────────────────────────────────────────

    pub fn eval(&mut self, e: &Expr) -> RunResult<Val> {
        self.burn(1)?;
        match e {
            Expr::Float(f) => Ok(Val::F(vec![*f])),
            Expr::Int(i) => Ok(Val::I(vec![*i])),
            Expr::UInt(u) => Ok(Val::U(vec![*u])),
            Expr::Bool(b) => Ok(Val::B(vec![*b])),
            Expr::Ident(name) => match self.get(name) {
                Some(v) => Ok(v.clone()),
                None => Err(RunError::Unsupported(format!("undeclared identifier '{name}'"))),
            },
            Expr::Seq(a, b) => {
                self.eval(a)?;
                self.eval(b)
            }
            Expr::Unary(op, a) => {
                let v = self.eval(a)?;
                Ok(unary(*op, &v))
            }
            Expr::Binary(BinOp::AndAnd, a, b) => {
                // Short-circuit, because GLSL does and because the right side may divide by zero.
                if !self.eval(a)?.as_bool() {
                    return Ok(Val::B(vec![false]));
                }
                Ok(Val::B(vec![self.eval(b)?.as_bool()]))
            }
            Expr::Binary(BinOp::OrOr, a, b) => {
                if self.eval(a)?.as_bool() {
                    return Ok(Val::B(vec![true]));
                }
                Ok(Val::B(vec![self.eval(b)?.as_bool()]))
            }
            Expr::Binary(op, a, b) => {
                let (x, y) = (self.eval(a)?, self.eval(b)?);
                binary(*op, &x, &y)
            }
            Expr::Cond(c, a, b) => {
                if self.eval(c)?.as_bool() {
                    self.eval(a)
                } else {
                    self.eval(b)
                }
            }
            Expr::Field(base, field) => {
                let v = self.eval(base)?;
                field_of(&v, field)
            }
            Expr::Index(base, idx) => {
                let v = self.eval(base)?;
                let i = self.eval(idx)?.as_i();
                index_of(&v, i)
            }
            Expr::Assign(op, lhs, rhs) => {
                let r = self.eval(rhs)?;
                let newv = match op {
                    None => r,
                    Some(o) => {
                        let cur = self.eval(lhs)?;
                        binary(*o, &cur, &r)?
                    }
                };
                self.assign(lhs, newv.clone())?;
                Ok(newv)
            }
            Expr::IncDec { inc, post, target } => {
                let cur = self.eval(target)?;
                let one = match cur {
                    Val::F(_) => Val::F(vec![1.0]),
                    Val::U(_) => Val::U(vec![1]),
                    _ => Val::I(vec![1]),
                };
                let next = binary(if *inc { BinOp::Add } else { BinOp::Sub }, &cur, &one)?;
                self.assign(target, next.clone())?;
                Ok(if *post { cur } else { next })
            }
            Expr::Call(name, args) => self.call(name, args),
        }
    }

    /// Write `v` through an lvalue expression — an identifier, optionally followed by any chain of
    /// `.swizzle`, `.field` and `[i]`.
    fn assign(&mut self, lhs: &Expr, v: Val) -> RunResult<()> {
        // Walk down to the root identifier, collecting the accessor path on the way back up.
        let mut path: Vec<Step> = Vec::new();
        let mut cur = lhs;
        loop {
            match cur {
                Expr::Field(b, f) => {
                    path.push(Step::Field(f.clone()));
                    cur = b;
                }
                Expr::Index(b, i) => {
                    let idx = self.eval(i)?.as_i();
                    path.push(Step::Index(idx.max(0) as usize));
                    cur = b;
                }
                Expr::Ident(name) => {
                    let name = name.clone();
                    path.reverse();
                    let mut root = match self.get(&name) {
                        Some(r) => r.clone(),
                        // Assigning to a name nothing declared: `gl_FragColor` in a shader that never
                        // declared it, which is every fragment shader. Create it.
                        None => Val::Void,
                    };
                    write_path(&mut root, &path, v, 0)?;
                    for s in self.scopes.iter_mut().rev() {
                        if s.contains_key(&name) {
                            s.insert(name, root);
                            return Ok(());
                        }
                    }
                    self.globals.insert(name, root);
                    return Ok(());
                }
                _ => return Err(RunError::Unsupported("assignment to a non-lvalue".into())),
            }
        }
    }

    fn call(&mut self, name: &str, args: &[Expr]) -> RunResult<Val> {
        // A user function shadows nothing built in — GLSL forbids overloading builtins — so a
        // declared function wins and everything else falls through to constructors and builtins.
        if let Some(f) = self.shader.func(name, args.len()) {
            let f: Func = f.clone();
            let mut vals = Vec::with_capacity(args.len());
            for (i, a) in args.iter().enumerate() {
                let v = self.eval(a)?;
                let v = match f.params.get(i) {
                    Some(p) => coerce(&v, &p.ty),
                    None => v,
                };
                vals.push(v);
            }
            let mut frame = HashMap::new();
            for (p, v) in f.params.iter().zip(vals.iter()) {
                frame.insert(p.name.clone(), v.clone());
            }
            // A function body sees the globals and its own parameters, NOT the caller's locals.
            let saved = std::mem::take(&mut self.scopes);
            self.scopes.push(frame);
            let flow = self.exec_block(&f.body);
            let end_frame = self.scopes.pop().unwrap_or_default();
            self.scopes = saved;
            let flow = flow?;
            // `out` / `inout` parameters are copied back, which is how GLSL's call-by-value-return
            // works and what makes `void f(out vec3 v)` usable.
            for (p, a) in f.params.iter().zip(args.iter()) {
                if matches!(p.qual, Qual::Out | Qual::InOut) {
                    if let Some(v) = end_frame.get(&p.name) {
                        let v = v.clone();
                        self.assign(a, v)?;
                    }
                }
            }
            return Ok(match flow {
                Flow::Return(v) => coerce(&v, &f.ret),
                _ => Val::Void,
            });
        }
        let mut vals = Vec::with_capacity(args.len());
        for a in args {
            vals.push(self.eval(a)?);
        }
        if let Some(v) = construct(name, &vals, self)? {
            return Ok(v);
        }
        self.builtin(name, &vals)
    }

    fn builtin(&mut self, name: &str, a: &[Val]) -> RunResult<Val> {
        use super::builtins;
        builtins::call(self, name, a)
    }
}

/// Overwrite the slot `path` names inside `root` with `v`.
fn write_path(root: &mut Val, path: &[Step], v: Val, i: usize) -> RunResult<()> {
    if i == path.len() {
        *root = merge_assign(root, v);
        return Ok(());
    }
    match &path[i] {
        Step::Index(n) => {
            let n = *n;
            match root {
                Val::Arr(items) => {
                    if n >= items.len() {
                        return Err(RunError::Unsupported("array index out of range in an assignment".into()));
                    }
                    write_path(&mut items[n], path, v, i + 1)
                }
                Val::M { rows, v: m, .. } => {
                    // A matrix indexes by COLUMN, and assigning one replaces `rows` floats.
                    let r = *rows;
                    let start = n * r;
                    if start + r > m.len() {
                        return Err(RunError::Unsupported("matrix column out of range".into()));
                    }
                    let f = v.floats();
                    for k in 0..r {
                        m[start + k] = f.get(k).copied().unwrap_or(0.0);
                    }
                    Ok(())
                }
                Val::F(c) => set_components(c, &[n], &v.floats()),
                Val::I(c) => set_components(c, &[n], &v.ints()),
                Val::U(c) => {
                    let u: Vec<u32> = v.ints().iter().map(|&x| x as u32).collect();
                    set_components(c, &[n], &u)
                }
                Val::B(c) => set_components(c, &[n], &v.bools()),
                _ => Err(RunError::Unsupported("indexed assignment to a non-indexable value".into())),
            }
        }
        Step::Field(f) => {
            if let Val::Struct(fields) = root {
                if let Some(slot) = fields.iter_mut().find(|(n, _)| n == f) {
                    return write_path(&mut slot.1, path, v, i + 1);
                }
                return Err(RunError::Unsupported(format!("no field '{f}'")));
            }
            // A swizzle write: `v.xz = ...`, which scatters components.
            let idx = swizzle_indices(f).ok_or_else(|| RunError::Unsupported(format!("bad swizzle '.{f}'")))?;
            if i + 1 != path.len() {
                return Err(RunError::Unsupported("cannot index into a swizzle".into()));
            }
            match root {
                Val::F(c) => set_components(c, &idx, &v.floats()),
                Val::I(c) => set_components(c, &idx, &v.ints()),
                Val::U(c) => {
                    let u: Vec<u32> = v.ints().iter().map(|&x| x as u32).collect();
                    set_components(c, &idx, &u)
                }
                Val::B(c) => set_components(c, &idx, &v.bools()),
                _ => Err(RunError::Unsupported("swizzle assignment to a non-vector".into())),
            }
        }
    }
}

fn set_components<T: Copy>(dst: &mut Vec<T>, idx: &[usize], src: &[T]) -> RunResult<()> {
    for (k, &d) in idx.iter().enumerate() {
        // GLSL forbids writing past a vector's width; growing here would silently change the type.
        if d >= dst.len() {
            return Err(RunError::Unsupported("component index out of range".into()));
        }
        let s = if src.len() == 1 { src[0] } else { *src.get(k).unwrap_or(&src[0]) };
        dst[d] = s;
    }
    Ok(())
}

/// A whole-value assignment. It keeps the destination's *width* when the source is a scalar (`v =
/// 0.0` on a vec3 is not legal GLSL, but `gl_FragColor = vec4(...)` on an undeclared name has to
/// create the vec4), and otherwise takes the source outright.
fn merge_assign(dst: &Val, src: Val) -> Val {
    match (dst, &src) {
        (Val::Void, _) => src,
        (Val::F(d), Val::F(s)) if d.len() > 1 && s.len() == 1 => Val::F(vec![s[0]; d.len()]),
        (Val::I(d), Val::I(s)) if d.len() > 1 && s.len() == 1 => Val::I(vec![s[0]; d.len()]),
        _ => src,
    }
}

/// One step of an lvalue path: `.field` / `.swizzle` or `[i]`.
enum Step {
    Field(String),
    Index(usize),
}

// ── operators ─────────────────────────────────────────────────────────────────────────────────

fn unary(op: UnOp, a: &Val) -> Val {
    match op {
        UnOp::Plus => a.clone(),
        UnOp::Not => Val::B(a.bools().iter().map(|&b| !b).collect()),
        UnOp::BitNot => Val::I(a.ints().iter().map(|&i| !i).collect()),
        UnOp::Neg => match a {
            Val::I(v) => Val::I(v.iter().map(|&x| -x).collect()),
            Val::U(v) => Val::U(v.iter().map(|&x| x.wrapping_neg()).collect()),
            Val::M { cols, rows, v } => {
                Val::M { cols: *cols, rows: *rows, v: v.iter().map(|&x| -x).collect() }
            }
            _ => Val::F(a.floats().iter().map(|&x| -x).collect()),
        },
    }
}

fn is_int_like(v: &Val) -> bool {
    matches!(v, Val::I(_) | Val::U(_))
}

fn binary(op: BinOp, a: &Val, b: &Val) -> RunResult<Val> {
    use BinOp::*;
    match op {
        Lt | Gt | Le | Ge => {
            let (x, y) = (a.as_f(), b.as_f());
            let r = match op {
                Lt => x < y,
                Gt => x > y,
                Le => x <= y,
                _ => x >= y,
            };
            return Ok(Val::B(vec![r]));
        }
        Eq => return Ok(Val::B(vec![val_eq(a, b)])),
        Ne => return Ok(Val::B(vec![!val_eq(a, b)])),
        AndAnd => return Ok(Val::B(vec![a.as_bool() && b.as_bool()])),
        OrOr => return Ok(Val::B(vec![a.as_bool() || b.as_bool()])),
        XorXor => return Ok(Val::B(vec![a.as_bool() != b.as_bool()])),
        BitAnd | BitOr | BitXor | Shl | Shr => {
            let (x, y) = (a.ints(), b.ints());
            let n = x.len().max(y.len());
            let pick = |v: &Vec<i32>, i: usize| if v.len() == 1 { v[0] } else { *v.get(i).unwrap_or(&0) };
            let out = (0..n)
                .map(|i| {
                    let (p, q) = (pick(&x, i), pick(&y, i));
                    match op {
                        BitAnd => p & q,
                        BitOr => p | q,
                        BitXor => p ^ q,
                        Shl => p.wrapping_shl(q as u32),
                        _ => p.wrapping_shr(q as u32),
                    }
                })
                .collect();
            return Ok(Val::I(out));
        }
        _ => {}
    }

    // Matrix arithmetic is not componentwise: `mat4 * vec4` is a linear map, and `mat * mat` is a
    // product. Getting this wrong is the difference between a transformed triangle and a smear.
    match (op, a, b) {
        (Mul, Val::M { cols, rows, v }, other) if !matches!(other, Val::M { .. }) && other.len() == *cols => {
            let x = other.floats();
            let mut out = vec![0.0; *rows];
            for c in 0..*cols {
                for r in 0..*rows {
                    out[r] += v[c * rows + r] * x[c];
                }
            }
            return Ok(Val::F(out));
        }
        (Mul, other, Val::M { cols, rows, v }) if !matches!(other, Val::M { .. }) && other.len() == *rows => {
            let x = other.floats();
            let mut out = vec![0.0; *cols];
            for c in 0..*cols {
                let mut s = 0.0;
                for r in 0..*rows {
                    s += v[c * rows + r] * x[r];
                }
                out[c] = s;
            }
            return Ok(Val::F(out));
        }
        (
            Mul,
            Val::M { cols: ac, rows: ar, v: av },
            Val::M { cols: bc, rows: br, v: bv },
        ) if ac == br => {
            let mut out = vec![0.0; bc * ar];
            for c in 0..*bc {
                for r in 0..*ar {
                    let mut s = 0.0;
                    for k in 0..*ac {
                        s += av[k * ar + r] * bv[c * br + k];
                    }
                    out[c * ar + r] = s;
                }
            }
            return Ok(Val::M { cols: *bc, rows: *ar, v: out });
        }
        _ => {}
    }

    // Everything else is componentwise with scalar broadcast. Integer operands stay integer (so
    // `1/2` is 0, as GLSL says); a float anywhere promotes the result.
    let int_math = is_int_like(a) && is_int_like(b);
    if int_math {
        let (x, y) = (a.ints(), b.ints());
        let n = x.len().max(y.len());
        let pick = |v: &Vec<i32>, i: usize| if v.len() == 1 { v[0] } else { *v.get(i).unwrap_or(&0) };
        let out: Vec<i32> = (0..n)
            .map(|i| {
                let (p, q) = (pick(&x, i), pick(&y, i));
                match op {
                    Add => p.wrapping_add(q),
                    Sub => p.wrapping_sub(q),
                    Mul => p.wrapping_mul(q),
                    Div => {
                        if q == 0 {
                            0
                        } else {
                            p.wrapping_div(q)
                        }
                    }
                    Mod => {
                        if q == 0 {
                            0
                        } else {
                            p.wrapping_rem(q)
                        }
                    }
                    _ => 0,
                }
            })
            .collect();
        return Ok(if matches!(a, Val::U(_)) && matches!(b, Val::U(_)) {
            Val::U(out.iter().map(|&i| i as u32).collect())
        } else {
            Val::I(out)
        });
    }
    let (x, y) = (a.floats(), b.floats());
    let n = x.len().max(y.len());
    let pick = |v: &Vec<f32>, i: usize| if v.len() == 1 { v[0] } else { *v.get(i).unwrap_or(&0.0) };
    let out: Vec<f32> = (0..n)
        .map(|i| {
            let (p, q) = (pick(&x, i), pick(&y, i));
            match op {
                Add => p + q,
                Sub => p - q,
                Mul => p * q,
                Div => p / q,
                Mod => p - q * (p / q).floor(),
                _ => 0.0,
            }
        })
        .collect();
    // A matrix keeps its shape through scalar and componentwise arithmetic.
    Ok(match (a, b) {
        (Val::M { cols, rows, .. }, _) | (_, Val::M { cols, rows, .. }) if out.len() == cols * rows => {
            Val::M { cols: *cols, rows: *rows, v: out }
        }
        _ => Val::F(out),
    })
}

fn val_eq(a: &Val, b: &Val) -> bool {
    match (a, b) {
        (Val::Struct(x), Val::Struct(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|((_, u), (_, v))| val_eq(u, v))
        }
        (Val::Arr(x), Val::Arr(y)) => x.len() == y.len() && x.iter().zip(y).all(|(u, v)| val_eq(u, v)),
        (Val::B(x), Val::B(y)) => x == y,
        _ => a.floats() == b.floats(),
    }
}

// ── swizzles, fields, indexing ────────────────────────────────────────────────────────────────

pub fn swizzle_indices(s: &str) -> Option<Vec<usize>> {
    if s.is_empty() || s.len() > 4 {
        return None;
    }
    let sets = ["xyzw", "rgba", "stpq"];
    let set = sets.iter().find(|set| s.chars().all(|c| set.contains(c)))?;
    Some(s.chars().map(|c| set.find(c).unwrap_or(0)).collect())
}

fn field_of(v: &Val, f: &str) -> RunResult<Val> {
    if let Val::Struct(fields) = v {
        return fields
            .iter()
            .find(|(n, _)| n == f)
            .map(|(_, val)| val.clone())
            .ok_or_else(|| RunError::Unsupported(format!("no field '{f}'")));
    }
    if f == "length" {
        // `.length()` reaches here as `.length` followed by a call with no args; the call falls
        // through to the builtin, so hand it the array itself.
        return Ok(v.clone());
    }
    let idx = swizzle_indices(f).ok_or_else(|| RunError::Unsupported(format!("bad swizzle '.{f}'")))?;
    let take = |n: usize, len: usize| -> RunResult<()> {
        if n >= len {
            Err(RunError::Unsupported(format!("swizzle '.{f}' out of range")))
        } else {
            Ok(())
        }
    };
    Ok(match v {
        Val::F(c) => {
            for &i in &idx {
                take(i, c.len())?;
            }
            Val::F(idx.iter().map(|&i| c[i]).collect())
        }
        Val::I(c) => {
            for &i in &idx {
                take(i, c.len())?;
            }
            Val::I(idx.iter().map(|&i| c[i]).collect())
        }
        Val::U(c) => {
            for &i in &idx {
                take(i, c.len())?;
            }
            Val::U(idx.iter().map(|&i| c[i]).collect())
        }
        Val::B(c) => {
            for &i in &idx {
                take(i, c.len())?;
            }
            Val::B(idx.iter().map(|&i| c[i]).collect())
        }
        _ => return Err(RunError::Unsupported(format!("'.{f}' on a non-vector"))),
    })
}

fn index_of(v: &Val, i: i32) -> RunResult<Val> {
    let n = i.max(0) as usize;
    Ok(match v {
        Val::Arr(items) => items
            .get(n)
            .cloned()
            .ok_or_else(|| RunError::Unsupported("array index out of range".into()))?,
        Val::M { rows, v: m, cols } => {
            if n >= *cols {
                return Err(RunError::Unsupported("matrix column out of range".into()));
            }
            Val::F(m[n * rows..(n + 1) * rows].to_vec())
        }
        Val::F(c) => Val::F(vec![*c.get(n).ok_or_else(oob)?]),
        Val::I(c) => Val::I(vec![*c.get(n).ok_or_else(oob)?]),
        Val::U(c) => Val::U(vec![*c.get(n).ok_or_else(oob)?]),
        Val::B(c) => Val::B(vec![*c.get(n).ok_or_else(oob)?]),
        _ => return Err(RunError::Unsupported("indexing a non-indexable value".into())),
    })
}

fn oob() -> RunError {
    RunError::Unsupported("component index out of range".into())
}

// ── constructors ──────────────────────────────────────────────────────────────────────────────

/// `vec3(...)`, `mat4(...)`, `float(...)`, `MyStruct(...)`, `float[3](...)` — `None` when `name` is
/// not a type, so the caller can try builtins next.
fn construct(name: &str, args: &[Val], it: &Interp) -> RunResult<Option<Val>> {
    // The array form the parser encodes as `T[n]`.
    if let Some(open) = name.find('[') {
        let base = &name[..open];
        let n: usize = name[open + 1..name.len() - 1].parse().unwrap_or(0);
        let mut items: Vec<Val> = args.to_vec();
        if n > 0 {
            items.resize(n, items.last().cloned().unwrap_or(Val::F(vec![0.0])));
        }
        let _ = base;
        return Ok(Some(Val::Arr(items)));
    }
    if let Some(fields) = it.shader.structs.get(name) {
        let vals = fields
            .iter()
            .enumerate()
            .map(|(i, (ft, fname))| {
                let v = args.get(i).cloned().unwrap_or_else(|| it.zero(ft));
                (fname.clone(), coerce(&v, ft))
            })
            .collect();
        return Ok(Some(Val::Struct(vals)));
    }
    let ty = match name {
        "float" => Type::Float,
        "int" => Type::Int,
        "uint" => Type::UInt,
        "bool" => Type::Bool,
        "vec2" => Type::Vec(2),
        "vec3" => Type::Vec(3),
        "vec4" => Type::Vec(4),
        "ivec2" => Type::IVec(2),
        "ivec3" => Type::IVec(3),
        "ivec4" => Type::IVec(4),
        "uvec2" => Type::UVec(2),
        "uvec3" => Type::UVec(3),
        "uvec4" => Type::UVec(4),
        "bvec2" => Type::BVec(2),
        "bvec3" => Type::BVec(3),
        "bvec4" => Type::BVec(4),
        "mat2" | "mat2x2" => Type::Mat(2, 2),
        "mat3" | "mat3x3" => Type::Mat(3, 3),
        "mat4" | "mat4x4" => Type::Mat(4, 4),
        "mat2x3" => Type::Mat(2, 3),
        "mat2x4" => Type::Mat(2, 4),
        "mat3x2" => Type::Mat(3, 2),
        "mat3x4" => Type::Mat(3, 4),
        "mat4x2" => Type::Mat(4, 2),
        "mat4x3" => Type::Mat(4, 3),
        _ => return Ok(None),
    };
    Ok(Some(build(&ty, args)))
}

/// Build a value of `ty` from constructor arguments, following GLSL's flattening rule: the
/// arguments' components are concatenated and consumed in order.
fn build(ty: &Type, args: &[Val]) -> Val {
    let n = ty.components();
    match ty {
        Type::Float => Val::F(vec![args.first().map(|v| v.as_f()).unwrap_or(0.0)]),
        Type::Int => Val::I(vec![args.first().map(|v| v.as_i()).unwrap_or(0)]),
        Type::UInt => Val::U(vec![args.first().map(|v| v.as_i() as u32).unwrap_or(0)]),
        Type::Bool => Val::B(vec![args.first().map(|v| v.as_bool()).unwrap_or(false)]),
        Type::Mat(c, r) => {
            // `matN(m)` resizes another matrix, keeping the identity outside the overlap.
            if args.len() == 1 {
                if let Val::M { cols: sc, rows: sr, v } = &args[0] {
                    let mut out = vec![0.0; c * r];
                    for i in 0..*c.min(r) {
                        out[i * r + i] = 1.0;
                    }
                    for col in 0..(*c).min(*sc) {
                        for row in 0..(*r).min(*sr) {
                            out[col * r + row] = v[col * sr + row];
                        }
                    }
                    return Val::M { cols: *c, rows: *r, v: out };
                }
                // `matN(x)` is the diagonal matrix.
                if args[0].len() == 1 {
                    let d = args[0].as_f();
                    let mut out = vec![0.0; c * r];
                    for i in 0..*c.min(r) {
                        out[i * r + i] = d;
                    }
                    return Val::M { cols: *c, rows: *r, v: out };
                }
            }
            let flat = flatten_f(args, n);
            Val::M { cols: *c, rows: *r, v: flat }
        }
        Type::Vec(_) => Val::F(flatten_f(args, n)),
        Type::IVec(_) => {
            let f = flatten_f(args, n);
            Val::I(f.iter().map(|&x| x as i32).collect())
        }
        Type::UVec(_) => {
            let f = flatten_f(args, n);
            Val::U(f.iter().map(|&x| x as u32).collect())
        }
        Type::BVec(_) => {
            let f = flatten_f(args, n);
            Val::B(f.iter().map(|&x| x != 0.0).collect())
        }
        _ => Val::Void,
    }
}

fn flatten_f(args: &[Val], want: usize) -> Vec<f32> {
    let mut flat: Vec<f32> = Vec::with_capacity(want);
    for a in args {
        flat.extend(a.floats());
    }
    if flat.len() == 1 && want > 1 {
        return vec![flat[0]; want];
    }
    flat.resize(want, 0.0);
    flat
}

/// Re-shape a value to a declared type — what a declaration's initializer and a function's argument
/// and return value all go through, so `float x = 1;` and `return 0;` from a `float` function work
/// the way a permissive driver accepts them.
pub fn coerce(v: &Val, ty: &Type) -> Val {
    match ty {
        Type::Float => Val::F(vec![v.as_f()]),
        Type::Int | Type::Sampler2D | Type::SamplerCube => Val::I(vec![v.as_i()]),
        Type::UInt => Val::U(vec![v.as_i() as u32]),
        Type::Bool => Val::B(vec![v.as_bool()]),
        Type::Vec(n) => {
            let mut f = v.floats();
            if f.len() == 1 {
                f = vec![f[0]; *n];
            }
            f.resize(*n, 0.0);
            Val::F(f)
        }
        Type::IVec(n) => {
            let mut f = v.ints();
            f.resize(*n, 0);
            Val::I(f)
        }
        Type::UVec(n) => {
            let mut f: Vec<u32> = v.ints().iter().map(|&x| x as u32).collect();
            f.resize(*n, 0);
            Val::U(f)
        }
        Type::BVec(n) => {
            let mut f = v.bools();
            f.resize(*n, false);
            Val::B(f)
        }
        Type::Mat(c, r) => match v {
            Val::M { .. } => v.clone(),
            _ => {
                let mut f = v.floats();
                f.resize(c * r, 0.0);
                Val::M { cols: *c, rows: *r, v: f }
            }
        },
        Type::Void => Val::Void,
        _ => v.clone(),
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::gl::glsl::compile;

    fn run(src: &str, set: &[(&str, Val)], read: &str) -> Val {
        let sh = Rc::new(compile(src).unwrap());
        let tex = NoTextures;
        let mut it = Interp::new(&sh, &tex, 5_000_000);
        it.init_globals().unwrap();
        for (k, v) in set {
            it.set(k, v.clone());
        }
        it.run_main().unwrap();
        it.get(read).cloned().unwrap_or(Val::Void)
    }

    #[test]
    fn arithmetic_and_swizzles() {
        let v = run(
            "void main() { vec4 c = vec4(1.0, 2.0, 3.0, 4.0); gl_FragColor = c.wzyx * 2.0; }",
            &[],
            "gl_FragColor",
        );
        assert_eq!(v.floats(), vec![8.0, 6.0, 4.0, 2.0]);
    }

    #[test]
    fn matrix_times_vector_is_a_linear_map_not_componentwise() {
        // A 90-degree rotation about z maps (1,0,0) to (0,1,0).
        let v = run(
            "void main() {
                mat3 m = mat3(0.0, 1.0, 0.0,  -1.0, 0.0, 0.0,  0.0, 0.0, 1.0);
                gl_FragColor = vec4(m * vec3(1.0, 0.0, 0.0), 1.0);
             }",
            &[],
            "gl_FragColor",
        );
        let f = v.floats();
        assert!((f[0] - 0.0).abs() < 1e-6 && (f[1] - 1.0).abs() < 1e-6, "{f:?}");
    }

    #[test]
    fn uniforms_reach_the_output() {
        let v = run(
            "uniform vec3 u; void main() { gl_FragColor = vec4(u, 1.0); }",
            &[("u", Val::F(vec![0.25, 0.5, 0.75]))],
            "gl_FragColor",
        );
        assert_eq!(v.floats(), vec![0.25, 0.5, 0.75, 1.0]);
    }

    #[test]
    fn loops_and_functions() {
        let v = run(
            "float sq(float x) { return x * x; }
             void main() { float s = 0.0; for (int i = 1; i <= 3; i++) { s += sq(float(i)); }
                           gl_FragColor = vec4(s); }",
            &[],
            "gl_FragColor",
        );
        assert_eq!(v.floats()[0], 14.0);
    }

    #[test]
    fn integer_division_truncates_like_glsl() {
        let v = run("void main() { int a = 7 / 2; gl_FragColor = vec4(float(a)); }", &[], "gl_FragColor");
        assert_eq!(v.floats()[0], 3.0);
    }

    #[test]
    fn a_runaway_loop_runs_out_of_fuel_instead_of_hanging() {
        let sh = Rc::new(compile("void main() { for (int i = 0; i < 1000000000; i++) { } }").unwrap());
        let tex = NoTextures;
        let mut it = Interp::new(&sh, &tex, 10_000);
        it.init_globals().unwrap();
        assert_eq!(it.run_main(), Err(RunError::OutOfFuel));
    }

    #[test]
    fn out_parameters_are_copied_back() {
        let v = run(
            "void f(out vec2 o) { o = vec2(3.0, 4.0); }
             void main() { vec2 p; f(p); gl_FragColor = vec4(p, 0.0, 1.0); }",
            &[],
            "gl_FragColor",
        );
        assert_eq!(v.floats(), vec![3.0, 4.0, 0.0, 1.0]);
    }

    #[test]
    fn swizzle_assignment_scatters() {
        let v = run(
            "void main() { vec4 c = vec4(0.0); c.rb = vec2(1.0, 0.5); gl_FragColor = c; }",
            &[],
            "gl_FragColor",
        );
        assert_eq!(v.floats(), vec![1.0, 0.0, 0.5, 0.0]);
    }

    #[test]
    fn structs_round_trip() {
        let v = run(
            "struct S { vec2 a; float b; };
             void main() { S s = S(vec2(1.0, 2.0), 3.0); gl_FragColor = vec4(s.a, s.b, 1.0); }",
            &[],
            "gl_FragColor",
        );
        assert_eq!(v.floats(), vec![1.0, 2.0, 3.0, 1.0]);
    }
}
