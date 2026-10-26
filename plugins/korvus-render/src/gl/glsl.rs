//! GLSL ES — the lexer, preprocessor and parser behind `compileShader`.
//!
//! A WebGL context that answers `COMPILE_STATUS` without a compiler is answering a question it has
//! not asked, and `drawArrays` on a program it never understood can only paint a fixed colour. So
//! this is a real front end: it tokenizes, expands the object-like macros shaders in the wild
//! actually use, and builds an AST that `interp.rs` walks per vertex and per fragment.
//!
//! It targets **GLSL ES 1.00** (WebGL 1) and accepts the ES 3.00 spellings WebGL 2 shaders use
//! (`in`/`out`/`flat`, `texture()`, the unsigned types), because one grammar that accepts both is
//! smaller and more honest than two that half-cover each.
//!
//! It is deliberately PERMISSIVE where the spec is strict — implicit `int` -> `float` in arithmetic,
//! no precision-qualifier checking, no redeclaration errors. A stricter compiler would reject
//! shaders a browser accepts, and a shader we refuse is a shader whose pixels we cannot produce,
//! which is a worse lie than accepting one too many. What it does NOT do is accept something it
//! cannot execute: a construct that reaches the interpreter unrecognised is a compile error with a
//! message, not a silent black frame.

use std::collections::HashMap;
use std::fmt::Write as _;

// ── types ─────────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    Void,
    Bool,
    Int,
    UInt,
    Float,
    /// `vec2`/`vec3`/`vec4`.
    Vec(usize),
    IVec(usize),
    UVec(usize),
    BVec(usize),
    /// `mat<cols>x<rows>`; `matN` is `Mat(N, N)`.
    Mat(usize, usize),
    Sampler2D,
    SamplerCube,
    Struct(String),
    /// `T name[n]` — the element type and the length (`0` = unsized, which only a function
    /// parameter or an ES3 `in`/`out` block may be).
    Array(Box<Type>, usize),
}

impl Type {
    /// How many scalar components one value of this type holds (0 for opaque/aggregate types).
    pub fn components(&self) -> usize {
        match self {
            Type::Bool | Type::Int | Type::UInt | Type::Float => 1,
            Type::Vec(n) | Type::IVec(n) | Type::UVec(n) | Type::BVec(n) => *n,
            Type::Mat(c, r) => c * r,
            _ => 0,
        }
    }

    /// The GL enum a `WebGLActiveInfo.type` reports for this type.
    pub fn gl_enum(&self) -> u32 {
        match self {
            Type::Float => 0x1406,          // FLOAT
            Type::Vec(2) => 0x8B50,         // FLOAT_VEC2
            Type::Vec(3) => 0x8B51,
            Type::Vec(4) => 0x8B52,
            Type::Int => 0x1404,            // INT
            Type::IVec(2) => 0x8B53,
            Type::IVec(3) => 0x8B54,
            Type::IVec(4) => 0x8B55,
            Type::UInt => 0x1405,           // UNSIGNED_INT
            Type::UVec(2) => 0x8DC6,
            Type::UVec(3) => 0x8DC7,
            Type::UVec(4) => 0x8DC8,
            Type::Bool => 0x8B56,           // BOOL
            Type::BVec(2) => 0x8B57,
            Type::BVec(3) => 0x8B58,
            Type::BVec(4) => 0x8B59,
            Type::Mat(2, 2) => 0x8B5A,      // FLOAT_MAT2
            Type::Mat(3, 3) => 0x8B5B,
            Type::Mat(4, 4) => 0x8B5C,
            Type::Mat(2, 3) => 0x8B65,
            Type::Mat(2, 4) => 0x8B66,
            Type::Mat(3, 2) => 0x8B67,
            Type::Mat(3, 4) => 0x8B68,
            Type::Mat(4, 2) => 0x8B69,
            Type::Mat(4, 3) => 0x8B6A,
            Type::Sampler2D => 0x8B5E,      // SAMPLER_2D
            Type::SamplerCube => 0x8B60,    // SAMPLER_CUBE
            Type::Array(inner, _) => inner.gl_enum(),
            _ => 0,
        }
    }
}

/// The storage qualifier a global declaration carries. `In`/`Out` are the ES 3.00 spellings of
/// `attribute`/`varying`, resolved by shader stage at link time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Qual {
    None,
    Const,
    Attribute,
    Uniform,
    Varying,
    In,
    Out,
    InOut,
}

#[derive(Debug, Clone)]
pub struct Decl {
    pub qual: Qual,
    pub ty: Type,
    pub name: String,
    pub init: Option<Expr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
    AndAnd,
    OrOr,
    XorXor,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    Plus,
    BitNot,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Float(f32),
    Int(i32),
    UInt(u32),
    Bool(bool),
    Ident(String),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// `a = b` (`op` `None`) or `a op= b`.
    Assign(Option<BinOp>, Box<Expr>, Box<Expr>),
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
    /// A function call OR a constructor — they are the same syntax, and which one it is depends on
    /// whether the name is a type, which the interpreter already knows.
    Call(String, Vec<Expr>),
    /// `.xyz` (swizzle) or `.field` (struct member).
    Field(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
    /// `++x` / `--x` / `x++` / `x--`; `post` says which value the expression evaluates to.
    IncDec { inc: bool, post: bool, target: Box<Expr> },
    /// The comma operator.
    Seq(Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Expr(Expr),
    Decls(Vec<Decl>),
    Block(Vec<Stmt>),
    If(Expr, Box<Stmt>, Option<Box<Stmt>>),
    For { init: Option<Box<Stmt>>, cond: Option<Expr>, step: Option<Expr>, body: Box<Stmt> },
    While(Expr, Box<Stmt>),
    DoWhile(Box<Stmt>, Expr),
    Return(Option<Expr>),
    Break,
    Continue,
    Discard,
    Empty,
    /// GLSL ES 3.00 `switch (sel) { case c: ... default: ... }` (zed.dev's and
    /// replicate.com's shaders failed to compile on it). Each arm is a label -- `None` for `default` --
    /// and the statements up to the next label; execution falls through arms until a `break`.
    Switch(Expr, Vec<(Option<Expr>, Vec<Stmt>)>),
}

#[derive(Debug, Clone)]
pub struct Param {
    pub qual: Qual,
    pub ty: Type,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Func {
    pub name: String,
    pub ret: Type,
    pub params: Vec<Param>,
    pub body: Vec<Stmt>,
}

/// A compiled shader: the declarations the linker needs and the function bodies the interpreter
/// walks.
#[derive(Debug, Clone, Default)]
pub struct Shader {
    pub globals: Vec<Decl>,
    pub funcs: Vec<Func>,
    pub structs: HashMap<String, Vec<(Type, String)>>,
    /// `#version 300 es` — the ES 3.00 spellings become the defaults for `in`/`out` resolution.
    pub es3: bool,
    /// The uniform interface blocks (`uniform Blk { vec4 u; };`), by block name, with their members in
    /// declaration order. The members are ALSO in `globals` (hoisted, so the interpreter reads them by
    /// bare name); this is what the program's block introspection reads.
    pub blocks: Vec<(String, Vec<(Type, String)>)>,
}

impl Shader {
    pub fn func(&self, name: &str, argc: usize) -> Option<&Func> {
        self.funcs
            .iter()
            .find(|f| f.name == name && f.params.len() == argc)
            .or_else(|| self.funcs.iter().find(|f| f.name == name))
    }
}

// ── lexing ────────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Float(f32),
    Int(i32),
    UInt(u32),
    Punct(&'static str),
    Eof,
}

/// Every multi-character operator, longest first — the lexer tries them in this order, so `<<=`
/// wins over `<<` which wins over `<`.
const PUNCTS: &[&str] = &[
    "<<=", ">>=", "++", "--", "<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "^^", "+=", "-=",
    "*=", "/=", "%=", "&=", "|=", "^=", "(", ")", "[", "]", "{", "}", ".", ",", ":", ";", "=", "!",
    "-", "+", "*", "/", "%", "<", ">", "&", "|", "^", "~", "?",
];

/// Strip comments, keeping newlines so `#` directives still line up.
fn strip_comments(src: &str) -> String {
    let b: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == '/' && i + 1 < b.len() && b[i + 1] == '/' {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
        } else if b[i] == '/' && i + 1 < b.len() && b[i + 1] == '*' {
            i += 2;
            while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                if b[i] == '\n' {
                    out.push('\n');
                }
                i += 1;
            }
            i = (i + 2).min(b.len());
            out.push(' ');
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// The preprocessor: `#version`, `#define`/`#undef` (object-like), `#ifdef`/`#ifndef`/`#if
/// defined(X)`/`#else`/`#elif`/`#endif`, and `#extension`/`#pragma`/`#line` as no-ops.
///
/// Function-like macros are NOT expanded — they appear in engine shaders (three.js) but never in the
/// fingerprint probes this path exists for, and a half-working macro expander that silently
/// mis-substitutes is worse than one that declines. A shader that needs one fails to compile with a
/// message saying so, which is a state the page can see and act on.
struct Preproc {
    defines: HashMap<String, String>,
    es3: bool,
}

impl Preproc {
    fn run(src: &str) -> Result<(String, bool), String> {
        let mut p = Preproc { defines: HashMap::new(), es3: false };
        let mut out = String::with_capacity(src.len());
        // `active` is the whole enclosing state; `taken` records whether a branch of the current
        // #if chain already ran, so #elif after a taken branch stays off.
        let mut stack: Vec<(bool, bool)> = Vec::new();
        for raw in src.lines() {
            let line = raw.trim_start();
            let active = stack.iter().all(|(a, _)| *a);
            if !line.starts_with('#') {
                if active {
                    out.push_str(&p.expand(raw));
                }
                out.push('\n');
                continue;
            }
            let rest = line[1..].trim();
            let (dir, arg) = match rest.find(|c: char| c.is_whitespace()) {
                Some(i) => (&rest[..i], rest[i..].trim()),
                None => (rest, ""),
            };
            match dir {
                "version" => {
                    if arg.starts_with("300") {
                        p.es3 = true;
                    }
                }
                "extension" | "pragma" | "line" | "" => {}
                "error" => {
                    if active {
                        return Err(format!("#error {arg}"));
                    }
                }
                "define" => {
                    if active {
                        let (name, body) = match arg.find(|c: char| c.is_whitespace() || c == '(') {
                            Some(i) => (&arg[..i], arg[i..].trim()),
                            None => (arg, ""),
                        };
                        if body.starts_with('(') {
                            return Err(format!(
                                "unsupported function-like macro '{name}' (korvus GLSL preprocessor expands object-like macros only)"
                            ));
                        }
                        p.defines.insert(name.to_string(), body.to_string());
                    }
                }
                "undef" => {
                    if active {
                        p.defines.remove(arg);
                    }
                }
                "ifdef" => {
                    let on = active && p.defines.contains_key(arg);
                    stack.push((on, on));
                }
                "ifndef" => {
                    let on = active && !p.defines.contains_key(arg);
                    stack.push((on, on));
                }
                "if" => {
                    let on = active && p.cond(arg);
                    stack.push((on, on));
                }
                "elif" => {
                    if let Some((a, taken)) = stack.pop() {
                        let outer = stack.iter().all(|(a, _)| *a);
                        let _ = a;
                        let on = outer && !taken && p.cond(arg);
                        stack.push((on, taken || on));
                    }
                }
                "else" => {
                    if let Some((a, taken)) = stack.pop() {
                        let outer = stack.iter().all(|(a, _)| *a);
                        let _ = a;
                        let on = outer && !taken;
                        stack.push((on, true));
                    }
                }
                "endif" => {
                    stack.pop();
                }
                other => return Err(format!("unknown preprocessor directive '#{other}'")),
            }
            out.push('\n');
        }
        Ok((out, p.es3))
    }

    /// `#if` conditions, over the forms shaders use: `defined(X)`, `X`, integer literals, `!`, `&&`,
    /// `||`, and integer comparisons of defined constants.
    fn cond(&self, arg: &str) -> bool {
        let expanded = {
            let mut s = String::new();
            let mut rest = arg;
            while let Some(i) = rest.find("defined") {
                s.push_str(&rest[..i]);
                let after = rest[i + 7..].trim_start();
                let (name, tail) = if let Some(a) = after.strip_prefix('(') {
                    match a.find(')') {
                        Some(j) => (a[..j].trim(), &a[j + 1..]),
                        None => (a.trim(), ""),
                    }
                } else {
                    match after.find(|c: char| !(c.is_alphanumeric() || c == '_')) {
                        Some(j) => (&after[..j], &after[j..]),
                        None => (after, ""),
                    }
                };
                s.push(if self.defines.contains_key(name) { '1' } else { '0' });
                rest = tail;
            }
            s.push_str(rest);
            self.expand(&s)
        };
        eval_pp(&expanded).unwrap_or(0) != 0
    }

    /// Substitute object-like macros, token by token, up to a small fixed depth (a macro whose body
    /// names itself would otherwise not terminate).
    fn expand(&self, line: &str) -> String {
        let mut cur = line.to_string();
        for _ in 0..8 {
            let mut out = String::with_capacity(cur.len());
            let mut changed = false;
            let ch: Vec<char> = cur.chars().collect();
            let mut i = 0;
            while i < ch.len() {
                if ch[i].is_alphabetic() || ch[i] == '_' {
                    let start = i;
                    while i < ch.len() && (ch[i].is_alphanumeric() || ch[i] == '_') {
                        i += 1;
                    }
                    let word: String = ch[start..i].iter().collect();
                    match self.defines.get(&word) {
                        Some(b) => {
                            out.push_str(b);
                            changed = true;
                        }
                        None => out.push_str(&word),
                    }
                } else {
                    out.push(ch[i]);
                    i += 1;
                }
            }
            cur = out;
            if !changed {
                break;
            }
        }
        cur
    }
}

/// A tiny integer expression evaluator for `#if` — `||`, `&&`, comparisons, `+ - * /`, `!`, parens.
fn eval_pp(s: &str) -> Option<i64> {
    let toks: Vec<String> = {
        let mut v = Vec::new();
        let ch: Vec<char> = s.chars().collect();
        let mut i = 0;
        while i < ch.len() {
            if ch[i].is_whitespace() {
                i += 1;
            } else if ch[i].is_ascii_digit() {
                let st = i;
                while i < ch.len() && ch[i].is_ascii_digit() {
                    i += 1;
                }
                v.push(ch[st..i].iter().collect());
            } else if ch[i].is_alphabetic() || ch[i] == '_' {
                // An undefined identifier is 0, which is what the C preprocessor does.
                while i < ch.len() && (ch[i].is_alphanumeric() || ch[i] == '_') {
                    i += 1;
                }
                v.push("0".to_string());
            } else {
                let two: String = ch[i..(i + 2).min(ch.len())].iter().collect();
                if ["&&", "||", "==", "!=", "<=", ">="].contains(&two.as_str()) {
                    v.push(two);
                    i += 2;
                } else {
                    v.push(ch[i].to_string());
                    i += 1;
                }
            }
        }
        v
    };
    let mut pos = 0;
    let v = pp_or(&toks, &mut pos)?;
    Some(v)
}

fn pp_or(t: &[String], p: &mut usize) -> Option<i64> {
    let mut l = pp_and(t, p)?;
    while t.get(*p).map(|s| s == "||").unwrap_or(false) {
        *p += 1;
        let r = pp_and(t, p)?;
        l = ((l != 0) || (r != 0)) as i64;
    }
    Some(l)
}
fn pp_and(t: &[String], p: &mut usize) -> Option<i64> {
    let mut l = pp_cmp(t, p)?;
    while t.get(*p).map(|s| s == "&&").unwrap_or(false) {
        *p += 1;
        let r = pp_cmp(t, p)?;
        l = ((l != 0) && (r != 0)) as i64;
    }
    Some(l)
}
fn pp_cmp(t: &[String], p: &mut usize) -> Option<i64> {
    let mut l = pp_add(t, p)?;
    while let Some(op) = t.get(*p) {
        let o = op.as_str();
        if !["==", "!=", "<", ">", "<=", ">="].contains(&o) {
            break;
        }
        *p += 1;
        let r = pp_add(t, p)?;
        l = match o {
            "==" => (l == r) as i64,
            "!=" => (l != r) as i64,
            "<" => (l < r) as i64,
            ">" => (l > r) as i64,
            "<=" => (l <= r) as i64,
            _ => (l >= r) as i64,
        };
    }
    Some(l)
}
fn pp_add(t: &[String], p: &mut usize) -> Option<i64> {
    let mut l = pp_mul(t, p)?;
    while let Some(op) = t.get(*p) {
        match op.as_str() {
            "+" => {
                *p += 1;
                l += pp_mul(t, p)?;
            }
            "-" => {
                *p += 1;
                l -= pp_mul(t, p)?;
            }
            _ => break,
        }
    }
    Some(l)
}
fn pp_mul(t: &[String], p: &mut usize) -> Option<i64> {
    let mut l = pp_unary(t, p)?;
    while let Some(op) = t.get(*p) {
        match op.as_str() {
            "*" => {
                *p += 1;
                l *= pp_unary(t, p)?;
            }
            "/" => {
                *p += 1;
                let r = pp_unary(t, p)?;
                l = if r == 0 { 0 } else { l / r };
            }
            _ => break,
        }
    }
    Some(l)
}
fn pp_unary(t: &[String], p: &mut usize) -> Option<i64> {
    match t.get(*p).map(|s| s.as_str()) {
        Some("!") => {
            *p += 1;
            Some((pp_unary(t, p)? == 0) as i64)
        }
        Some("-") => {
            *p += 1;
            Some(-pp_unary(t, p)?)
        }
        Some("+") => {
            *p += 1;
            pp_unary(t, p)
        }
        Some("(") => {
            *p += 1;
            let v = pp_or(t, p)?;
            if t.get(*p).map(|s| s == ")").unwrap_or(false) {
                *p += 1;
            }
            Some(v)
        }
        Some(n) => {
            let v = n.parse::<i64>().ok()?;
            *p += 1;
            Some(v)
        }
        None => None,
    }
}

fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let ch: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < ch.len() {
        let c = ch[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let st = i;
            while i < ch.len() && (ch[i].is_alphanumeric() || ch[i] == '_') {
                i += 1;
            }
            out.push(Tok::Ident(ch[st..i].iter().collect()));
            continue;
        }
        if c.is_ascii_digit() || (c == '.' && i + 1 < ch.len() && ch[i + 1].is_ascii_digit()) {
            let st = i;
            let mut is_float = false;
            if c == '0' && i + 1 < ch.len() && (ch[i + 1] == 'x' || ch[i + 1] == 'X') {
                i += 2;
                while i < ch.len() && ch[i].is_ascii_hexdigit() {
                    i += 1;
                }
                let text: String = ch[st + 2..i].iter().collect();
                let v = u32::from_str_radix(&text, 16).map_err(|_| format!("bad hex literal '{text}'"))?;
                if i < ch.len() && (ch[i] == 'u' || ch[i] == 'U') {
                    i += 1;
                    out.push(Tok::UInt(v));
                } else {
                    out.push(Tok::Int(v as i32));
                }
                continue;
            }
            while i < ch.len() && ch[i].is_ascii_digit() {
                i += 1;
            }
            if i < ch.len() && ch[i] == '.' {
                is_float = true;
                i += 1;
                while i < ch.len() && ch[i].is_ascii_digit() {
                    i += 1;
                }
            }
            if i < ch.len() && (ch[i] == 'e' || ch[i] == 'E') {
                let save = i;
                i += 1;
                if i < ch.len() && (ch[i] == '+' || ch[i] == '-') {
                    i += 1;
                }
                if i < ch.len() && ch[i].is_ascii_digit() {
                    is_float = true;
                    while i < ch.len() && ch[i].is_ascii_digit() {
                        i += 1;
                    }
                } else {
                    i = save;
                }
            }
            let text: String = ch[st..i].iter().collect();
            if i < ch.len() && (ch[i] == 'f' || ch[i] == 'F') {
                i += 1;
                is_float = true;
            } else if i < ch.len() && (ch[i] == 'u' || ch[i] == 'U') && !is_float {
                i += 1;
                let v: u32 = text.parse().map_err(|_| format!("bad literal '{text}'"))?;
                out.push(Tok::UInt(v));
                continue;
            }
            if is_float {
                out.push(Tok::Float(text.parse::<f32>().map_err(|_| format!("bad literal '{text}'"))?));
            } else {
                // An integer literal too large for i32 wraps the way a GL compiler's does.
                let v = text.parse::<i64>().map_err(|_| format!("bad literal '{text}'"))?;
                out.push(Tok::Int(v as i32));
            }
            continue;
        }
        let mut matched = None;
        for p in PUNCTS {
            if ch[i..].len() >= p.len() && ch[i..i + p.len()].iter().collect::<String>() == **p {
                matched = Some(*p);
                break;
            }
        }
        match matched {
            Some(p) => {
                out.push(Tok::Punct(p));
                i += p.chars().count();
            }
            None => return Err(format!("unexpected character '{c}'")),
        }
    }
    out.push(Tok::Eof);
    Ok(out)
}

// ── parsing ───────────────────────────────────────────────────────────────────────────────────

struct Parser {
    t: Vec<Tok>,
    p: usize,
    structs: HashMap<String, Vec<(Type, String)>>,
}

const KEYWORDS_SKIPPABLE: &[&str] = &[
    "highp", "mediump", "lowp", "invariant", "flat", "smooth", "noperspective", "centroid",
    "precise", "coherent", "volatile", "restrict", "readonly", "writeonly",
];

impl Parser {
    fn peek(&self) -> &Tok {
        self.t.get(self.p).unwrap_or(&Tok::Eof)
    }
    fn at_punct(&self, s: &str) -> bool {
        matches!(self.peek(), Tok::Punct(p) if *p == s)
    }
    fn at_ident(&self, s: &str) -> bool {
        matches!(self.peek(), Tok::Ident(i) if i == s)
    }
    fn eat_punct(&mut self, s: &str) -> bool {
        if self.at_punct(s) {
            self.p += 1;
            true
        } else {
            false
        }
    }
    fn eat_ident(&mut self, s: &str) -> bool {
        if self.at_ident(s) {
            self.p += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, s: &str) -> Result<(), String> {
        if self.eat_punct(s) {
            Ok(())
        } else {
            Err(format!("expected '{s}', found {}", describe(self.peek())))
        }
    }
    fn ident(&mut self) -> Result<String, String> {
        match self.peek().clone() {
            Tok::Ident(i) => {
                self.p += 1;
                Ok(i)
            }
            t => Err(format!("expected an identifier, found {}", describe(&t))),
        }
    }

    fn skip_qualifier_noise(&mut self) {
        loop {
            let mut moved = false;
            for k in KEYWORDS_SKIPPABLE {
                if self.eat_ident(k) {
                    moved = true;
                }
            }
            // `layout(...)` — parsed and dropped; the interpreter assigns locations itself.
            if self.at_ident("layout") {
                self.p += 1;
                if self.eat_punct("(") {
                    let mut depth = 1;
                    while depth > 0 && !matches!(self.peek(), Tok::Eof) {
                        if self.at_punct("(") {
                            depth += 1;
                        } else if self.at_punct(")") {
                            depth -= 1;
                        }
                        self.p += 1;
                    }
                }
                moved = true;
            }
            if !moved {
                return;
            }
        }
    }

    fn qualifier(&mut self) -> Qual {
        self.skip_qualifier_noise();
        let q = if self.eat_ident("const") {
            Qual::Const
        } else if self.eat_ident("attribute") {
            Qual::Attribute
        } else if self.eat_ident("uniform") {
            Qual::Uniform
        } else if self.eat_ident("varying") {
            Qual::Varying
        } else if self.at_ident("inout") {
            self.p += 1;
            Qual::InOut
        } else if self.eat_ident("in") {
            Qual::In
        } else if self.eat_ident("out") {
            Qual::Out
        } else {
            Qual::None
        };
        self.skip_qualifier_noise();
        q
    }

    /// A type name, or `None` if the next token is not one.
    fn ty(&mut self) -> Option<Type> {
        self.skip_qualifier_noise();
        let name = match self.peek() {
            Tok::Ident(i) => i.clone(),
            _ => return None,
        };
        let t = match name.as_str() {
            "void" => Type::Void,
            "bool" => Type::Bool,
            "int" => Type::Int,
            "uint" => Type::UInt,
            "float" => Type::Float,
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
            "sampler2D" | "sampler2DShadow" | "sampler2DArray" | "isampler2D" | "usampler2D" => Type::Sampler2D,
            "samplerCube" | "samplerCubeShadow" => Type::SamplerCube,
            other if self.structs.contains_key(other) => Type::Struct(other.to_string()),
            _ => return None,
        };
        self.p += 1;
        Some(t)
    }

    /// `[n]` after a name or a type, with a constant-foldable length.
    fn array_suffix(&mut self) -> Result<Option<usize>, String> {
        if !self.eat_punct("[") {
            return Ok(None);
        }
        if self.eat_punct("]") {
            return Ok(Some(0));
        }
        let e = self.expr()?;
        self.expect("]")?;
        match const_int(&e) {
            Some(n) if n >= 0 => Ok(Some(n as usize)),
            _ => Err("array size must be a non-negative integer constant".into()),
        }
    }

    fn struct_def(&mut self) -> Result<(), String> {
        // `struct Name { ... };` — anonymous structs are not accepted (nothing could name the type).
        let name = self.ident()?;
        self.expect("{")?;
        let mut fields = Vec::new();
        while !self.at_punct("}") {
            if matches!(self.peek(), Tok::Eof) {
                return Err("unterminated struct".into());
            }
            let ty = self.ty().ok_or_else(|| format!("expected a type in struct {name}"))?;
            loop {
                let fname = self.ident()?;
                let arr = self.array_suffix()?;
                let fty = match arr {
                    Some(n) => Type::Array(Box::new(ty.clone()), n),
                    None => ty.clone(),
                };
                fields.push((fty, fname));
                if !self.eat_punct(",") {
                    break;
                }
            }
            self.expect(";")?;
        }
        self.expect("}")?;
        self.expect(";")?;
        self.structs.insert(name, fields);
        Ok(())
    }

    fn declaration(&mut self, qual: Qual, ty: Type) -> Result<Vec<Decl>, String> {
        let mut out = Vec::new();
        loop {
            let name = self.ident()?;
            let arr = self.array_suffix()?;
            let dty = match arr {
                Some(n) => Type::Array(Box::new(ty.clone()), n),
                None => ty.clone(),
            };
            let init = if self.eat_punct("=") { Some(self.assignment()?) } else { None };
            out.push(Decl { qual, ty: dty, name, init });
            if !self.eat_punct(",") {
                break;
            }
        }
        self.expect(";")?;
        Ok(out)
    }

    // --- expressions (precedence climbing) ---

    fn expr(&mut self) -> Result<Expr, String> {
        let mut e = self.assignment()?;
        while self.eat_punct(",") {
            let r = self.assignment()?;
            e = Expr::Seq(Box::new(e), Box::new(r));
        }
        Ok(e)
    }

    fn assignment(&mut self) -> Result<Expr, String> {
        let lhs = self.conditional()?;
        let op = match self.peek() {
            Tok::Punct("=") => Some(None),
            Tok::Punct("+=") => Some(Some(BinOp::Add)),
            Tok::Punct("-=") => Some(Some(BinOp::Sub)),
            Tok::Punct("*=") => Some(Some(BinOp::Mul)),
            Tok::Punct("/=") => Some(Some(BinOp::Div)),
            Tok::Punct("%=") => Some(Some(BinOp::Mod)),
            Tok::Punct("&=") => Some(Some(BinOp::BitAnd)),
            Tok::Punct("|=") => Some(Some(BinOp::BitOr)),
            Tok::Punct("^=") => Some(Some(BinOp::BitXor)),
            Tok::Punct("<<=") => Some(Some(BinOp::Shl)),
            Tok::Punct(">>=") => Some(Some(BinOp::Shr)),
            _ => None,
        };
        match op {
            Some(o) => {
                self.p += 1;
                let rhs = self.assignment()?;
                Ok(Expr::Assign(o, Box::new(lhs), Box::new(rhs)))
            }
            None => Ok(lhs),
        }
    }

    fn conditional(&mut self) -> Result<Expr, String> {
        let c = self.binary(0)?;
        if self.eat_punct("?") {
            let a = self.assignment()?;
            self.expect(":")?;
            let b = self.assignment()?;
            return Ok(Expr::Cond(Box::new(c), Box::new(a), Box::new(b)));
        }
        Ok(c)
    }

    fn binary(&mut self, min_prec: u8) -> Result<Expr, String> {
        let mut lhs = self.unary()?;
        loop {
            let (op, prec) = match self.peek() {
                Tok::Punct("||") => (BinOp::OrOr, 1),
                Tok::Punct("^^") => (BinOp::XorXor, 2),
                Tok::Punct("&&") => (BinOp::AndAnd, 3),
                Tok::Punct("|") => (BinOp::BitOr, 4),
                Tok::Punct("^") => (BinOp::BitXor, 5),
                Tok::Punct("&") => (BinOp::BitAnd, 6),
                Tok::Punct("==") => (BinOp::Eq, 7),
                Tok::Punct("!=") => (BinOp::Ne, 7),
                Tok::Punct("<") => (BinOp::Lt, 8),
                Tok::Punct(">") => (BinOp::Gt, 8),
                Tok::Punct("<=") => (BinOp::Le, 8),
                Tok::Punct(">=") => (BinOp::Ge, 8),
                Tok::Punct("<<") => (BinOp::Shl, 9),
                Tok::Punct(">>") => (BinOp::Shr, 9),
                Tok::Punct("+") => (BinOp::Add, 10),
                Tok::Punct("-") => (BinOp::Sub, 10),
                Tok::Punct("*") => (BinOp::Mul, 11),
                Tok::Punct("/") => (BinOp::Div, 11),
                Tok::Punct("%") => (BinOp::Mod, 11),
                _ => break,
            };
            if prec < min_prec {
                break;
            }
            self.p += 1;
            let rhs = self.binary(prec + 1)?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Result<Expr, String> {
        if self.eat_punct("-") {
            return Ok(Expr::Unary(UnOp::Neg, Box::new(self.unary()?)));
        }
        if self.eat_punct("+") {
            return Ok(Expr::Unary(UnOp::Plus, Box::new(self.unary()?)));
        }
        if self.eat_punct("!") {
            return Ok(Expr::Unary(UnOp::Not, Box::new(self.unary()?)));
        }
        if self.eat_punct("~") {
            return Ok(Expr::Unary(UnOp::BitNot, Box::new(self.unary()?)));
        }
        if self.eat_punct("++") {
            let t = self.unary()?;
            return Ok(Expr::IncDec { inc: true, post: false, target: Box::new(t) });
        }
        if self.eat_punct("--") {
            let t = self.unary()?;
            return Ok(Expr::IncDec { inc: false, post: false, target: Box::new(t) });
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Expr, String> {
        let mut e = self.primary()?;
        loop {
            if self.eat_punct(".") {
                let f = self.ident()?;
                e = Expr::Field(Box::new(e), f);
            } else if self.eat_punct("[") {
                let idx = self.expr()?;
                self.expect("]")?;
                e = Expr::Index(Box::new(e), Box::new(idx));
            } else if self.eat_punct("++") {
                e = Expr::IncDec { inc: true, post: true, target: Box::new(e) };
            } else if self.eat_punct("--") {
                e = Expr::IncDec { inc: false, post: true, target: Box::new(e) };
            } else {
                return Ok(e);
            }
        }
    }

    fn primary(&mut self) -> Result<Expr, String> {
        match self.peek().clone() {
            Tok::Float(f) => {
                self.p += 1;
                Ok(Expr::Float(f))
            }
            Tok::Int(i) => {
                self.p += 1;
                Ok(Expr::Int(i))
            }
            Tok::UInt(u) => {
                self.p += 1;
                Ok(Expr::UInt(u))
            }
            Tok::Punct("(") => {
                self.p += 1;
                let e = self.expr()?;
                self.expect(")")?;
                Ok(e)
            }
            Tok::Ident(name) => {
                self.p += 1;
                match name.as_str() {
                    "true" => return Ok(Expr::Bool(true)),
                    "false" => return Ok(Expr::Bool(false)),
                    _ => {}
                }
                // A constructor with an array type — `float[3](1.0, 2.0, 3.0)` — parses as a call
                // whose name carries the size; the interpreter handles both.
                if self.at_punct("[") {
                    let save = self.p;
                    if let Ok(Some(n)) = self.array_suffix() {
                        if self.at_punct("(") {
                            self.p += 1;
                            let args = self.call_args()?;
                            return Ok(Expr::Call(format!("{name}[{n}]"), args));
                        }
                    }
                    self.p = save;
                }
                if self.eat_punct("(") {
                    let args = self.call_args()?;
                    return Ok(Expr::Call(name, args));
                }
                Ok(Expr::Ident(name))
            }
            t => Err(format!("unexpected {}", describe(&t))),
        }
    }

    fn call_args(&mut self) -> Result<Vec<Expr>, String> {
        let mut args = Vec::new();
        if self.eat_punct(")") {
            return Ok(args);
        }
        // `f(void)` is the argument-less spelling.
        if self.at_ident("void") {
            let save = self.p;
            self.p += 1;
            if self.eat_punct(")") {
                return Ok(args);
            }
            self.p = save;
        }
        loop {
            args.push(self.assignment()?);
            if self.eat_punct(",") {
                continue;
            }
            self.expect(")")?;
            return Ok(args);
        }
    }

    // --- statements ---

    fn stmt(&mut self) -> Result<Stmt, String> {
        if self.eat_punct(";") {
            return Ok(Stmt::Empty);
        }
        if self.at_punct("{") {
            return Ok(Stmt::Block(self.block()?));
        }
        if self.eat_ident("if") {
            self.expect("(")?;
            let c = self.expr()?;
            self.expect(")")?;
            let then = self.stmt()?;
            let els = if self.eat_ident("else") { Some(Box::new(self.stmt()?)) } else { None };
            return Ok(Stmt::If(c, Box::new(then), els));
        }
        if self.eat_ident("for") {
            self.expect("(")?;
            let init = if self.at_punct(";") {
                self.p += 1;
                None
            } else {
                Some(Box::new(self.simple_stmt()?))
            };
            let cond = if self.at_punct(";") { None } else { Some(self.expr()?) };
            self.expect(";")?;
            let step = if self.at_punct(")") { None } else { Some(self.expr()?) };
            self.expect(")")?;
            let body = self.stmt()?;
            return Ok(Stmt::For { init, cond, step, body: Box::new(body) });
        }
        if self.eat_ident("while") {
            self.expect("(")?;
            let c = self.expr()?;
            self.expect(")")?;
            return Ok(Stmt::While(c, Box::new(self.stmt()?)));
        }
        if self.eat_ident("switch") {
            self.expect("(")?;
            let sel = self.expr()?;
            self.expect(")")?;
            self.expect("{")?;
            let mut arms: Vec<(Option<Expr>, Vec<Stmt>)> = Vec::new();
            loop {
                if self.eat_punct("}") {
                    break;
                }
                if matches!(self.peek(), Tok::Eof) {
                    return Err("expected '}' to close the switch".into());
                }
                if self.eat_ident("case") {
                    let label = self.expr()?;
                    self.expect(":")?;
                    arms.push((Some(label), Vec::new()));
                    continue;
                }
                if self.eat_ident("default") {
                    self.expect(":")?;
                    arms.push((None, Vec::new()));
                    continue;
                }
                let st = self.stmt()?;
                match arms.last_mut() {
                    Some(arm) => arm.1.push(st),
                    None => return Err("a statement before the first 'case' or 'default' in a switch".into()),
                }
            }
            return Ok(Stmt::Switch(sel, arms));
        }
        if self.eat_ident("do") {
            let body = self.stmt()?;
            if !self.eat_ident("while") {
                return Err("expected 'while' after 'do'".into());
            }
            self.expect("(")?;
            let c = self.expr()?;
            self.expect(")")?;
            self.expect(";")?;
            return Ok(Stmt::DoWhile(Box::new(body), c));
        }
        if self.eat_ident("return") {
            let v = if self.at_punct(";") { None } else { Some(self.expr()?) };
            self.expect(";")?;
            return Ok(Stmt::Return(v));
        }
        if self.eat_ident("break") {
            self.expect(";")?;
            return Ok(Stmt::Break);
        }
        if self.eat_ident("continue") {
            self.expect(";")?;
            return Ok(Stmt::Continue);
        }
        if self.eat_ident("discard") {
            self.expect(";")?;
            return Ok(Stmt::Discard);
        }
        if self.at_ident("precision") {
            // `precision mediump float;` — a real declaration with no effect on our arithmetic,
            // which is f32 throughout. Consumed so it does not parse as an expression.
            while !self.at_punct(";") && !matches!(self.peek(), Tok::Eof) {
                self.p += 1;
            }
            self.expect(";")?;
            return Ok(Stmt::Empty);
        }
        self.simple_stmt()
    }

    /// A declaration or an expression statement — the two things a `for` initializer can be.
    fn simple_stmt(&mut self) -> Result<Stmt, String> {
        let save = self.p;
        let qual = self.qualifier();
        if let Some(ty) = self.ty() {
            if matches!(self.peek(), Tok::Ident(_)) {
                return Ok(Stmt::Decls(self.declaration(qual, ty)?));
            }
        }
        self.p = save;
        let e = self.expr()?;
        self.expect(";")?;
        Ok(Stmt::Expr(e))
    }

    fn block(&mut self) -> Result<Vec<Stmt>, String> {
        self.expect("{")?;
        let mut out = Vec::new();
        while !self.at_punct("}") {
            if matches!(self.peek(), Tok::Eof) {
                return Err("unterminated block".into());
            }
            out.push(self.stmt()?);
        }
        self.expect("}")?;
        Ok(out)
    }
}

fn describe(t: &Tok) -> String {
    match t {
        Tok::Ident(i) => format!("'{i}'"),
        Tok::Float(f) => format!("'{f}'"),
        Tok::Int(i) => format!("'{i}'"),
        Tok::UInt(u) => format!("'{u}u'"),
        Tok::Punct(p) => format!("'{p}'"),
        Tok::Eof => "end of shader".to_string(),
    }
}

/// Constant-fold an integer expression, for array sizes.
fn const_int(e: &Expr) -> Option<i64> {
    Some(match e {
        Expr::Int(i) => *i as i64,
        Expr::UInt(u) => *u as i64,
        Expr::Unary(UnOp::Neg, a) => -const_int(a)?,
        Expr::Unary(UnOp::Plus, a) => const_int(a)?,
        Expr::Binary(op, a, b) => {
            let (a, b) = (const_int(a)?, const_int(b)?);
            match op {
                BinOp::Add => a + b,
                BinOp::Sub => a - b,
                BinOp::Mul => a * b,
                BinOp::Div if b != 0 => a / b,
                _ => return None,
            }
        }
        _ => return None,
    })
}

/// Compile one shader stage. `Err` is the string `getShaderInfoLog` returns and the reason
/// `COMPILE_STATUS` is `false`.
pub fn compile(source: &str) -> Result<Shader, String> {
    let stripped = strip_comments(source);
    let (pre, es3) = Preproc::run(&stripped)?;
    let toks = lex(&pre).map_err(|e| format!("ERROR: 0:0: {e}"))?;
    let mut p = Parser { t: toks, p: 0, structs: HashMap::new() };
    let mut sh = Shader { es3, ..Default::default() };
    loop {
        if matches!(p.peek(), Tok::Eof) {
            break;
        }
        if p.eat_punct(";") {
            continue;
        }
        if p.at_ident("precision") {
            while !p.at_punct(";") && !matches!(p.peek(), Tok::Eof) {
                p.p += 1;
            }
            p.expect(";").map_err(fmt_err)?;
            continue;
        }
        if p.at_ident("struct") {
            p.p += 1;
            p.struct_def().map_err(fmt_err)?;
            continue;
        }
        let qual = p.qualifier();
        // An ES3 interface block (`uniform Block { ... };`) — parsed and its members hoisted, which
        // is how a page reads them back by bare name anyway when there is no instance name.
        if qual == Qual::Uniform && matches!(p.peek(), Tok::Ident(_)) {
            let save = p.p;
            let blockname = p.ident().map_err(fmt_err)?;
            if p.at_punct("{") {
                let members = {
                    p.p += 1;
                    let mut out = Vec::new();
                    while !p.at_punct("}") {
                        if matches!(p.peek(), Tok::Eof) {
                            return Err(fmt_err("unterminated uniform block".to_string()));
                        }
                        let ty = p.ty().ok_or_else(|| fmt_err("expected a type in a uniform block".to_string()))?;
                        let decls = p.declaration(Qual::Uniform, ty).map_err(fmt_err)?;
                        out.extend(decls);
                    }
                    p.p += 1;
                    // An optional instance name, then `;`.
                    if matches!(p.peek(), Tok::Ident(_)) {
                        let _ = p.ident();
                    }
                    p.expect(";").map_err(fmt_err)?;
                    out
                };
                sh.blocks.push((blockname.clone(), members.iter().map(|d| (d.ty.clone(), d.name.clone())).collect()));
                sh.globals.extend(members);
                continue;
            }
            p.p = save;
        }
        let ty = match p.ty() {
            Some(t) => t,
            None => return Err(fmt_err(format!("expected a declaration, found {}", describe(p.peek())))),
        };
        // A bare `in;`/`out;` layout declaration.
        if p.eat_punct(";") {
            continue;
        }
        let name = p.ident().map_err(fmt_err)?;
        if p.at_punct("(") {
            p.p += 1;
            let mut params = Vec::new();
            if !p.eat_punct(")") {
                if p.at_ident("void") && matches!(p.t.get(p.p + 1), Some(Tok::Punct(")"))) {
                    p.p += 2;
                } else {
                    loop {
                        let pq = p.qualifier();
                        let pty = p.ty().ok_or_else(|| fmt_err("expected a parameter type".to_string()))?;
                        let pname = if matches!(p.peek(), Tok::Ident(_)) {
                            p.ident().map_err(fmt_err)?
                        } else {
                            String::new()
                        };
                        let arr = p.array_suffix().map_err(fmt_err)?;
                        let pty = match arr {
                            Some(n) => Type::Array(Box::new(pty), n),
                            None => pty,
                        };
                        params.push(Param { qual: pq, ty: pty, name: pname });
                        if p.eat_punct(",") {
                            continue;
                        }
                        p.expect(")").map_err(fmt_err)?;
                        break;
                    }
                }
            }
            if p.eat_punct(";") {
                // A prototype: nothing to record, the definition follows.
                continue;
            }
            let body = p.block().map_err(fmt_err)?;
            sh.funcs.push(Func { name, ret: ty, params, body });
            continue;
        }
        // A global variable declaration: rewind over the name so `declaration` reads it.
        p.p -= 1;
        let decls = p.declaration(qual, ty).map_err(fmt_err)?;
        sh.globals.extend(decls);
    }
    sh.structs = p.structs;
    if sh.func("main", 0).is_none() {
        return Err("ERROR: 0:0: 'main' : function does not return a value / is missing".into());
    }
    Ok(sh)
}

fn fmt_err(e: String) -> String {
    let mut s = String::new();
    let _ = write!(s, "ERROR: 0:0: {e}");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_minimal_vertex_shader() {
        let sh = compile(
            "attribute vec2 a_pos; uniform float u_s; varying vec3 v_c;
             void main() { v_c = vec3(a_pos, 1.0); gl_Position = vec4(a_pos * u_s, 0.0, 1.0); }",
        )
        .unwrap();
        assert_eq!(sh.globals.len(), 3);
        assert_eq!(sh.globals[0].qual, Qual::Attribute);
        assert_eq!(sh.globals[1].ty, Type::Float);
        assert!(sh.func("main", 0).is_some());
    }

    #[test]
    fn a_syntax_error_is_a_compile_error_not_a_panic() {
        let e = compile("void main( { }").unwrap_err();
        assert!(e.starts_with("ERROR:"), "{e}");
    }

    #[test]
    fn a_shader_without_main_does_not_compile() {
        assert!(compile("float f(float x) { return x; }").is_err());
    }

    #[test]
    fn preprocessor_conditionals_and_defines() {
        let sh = compile(
            "#define N 3\n#ifdef N\nuniform float u[N];\n#else\nuniform float u[1];\n#endif\nvoid main() {}",
        )
        .unwrap();
        assert_eq!(sh.globals[0].ty, Type::Array(Box::new(Type::Float), 3));
    }

    #[test]
    fn structs_and_arrays_parse() {
        let sh = compile(
            "struct Light { vec3 dir; float k; };
             uniform Light lights[2];
             void main() { }",
        )
        .unwrap();
        assert!(sh.structs.contains_key("Light"));
        assert_eq!(sh.globals[0].ty, Type::Array(Box::new(Type::Struct("Light".into())), 2));
    }

    #[test]
    fn es3_in_out_and_version() {
        let sh = compile("#version 300 es\nin vec4 p; out vec4 c; void main() { c = p; }").unwrap();
        assert!(sh.es3);
        assert_eq!(sh.globals[0].qual, Qual::In);
    }
}
