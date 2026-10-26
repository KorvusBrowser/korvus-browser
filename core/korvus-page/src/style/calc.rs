//! CSS MATH FUNCTIONS AS BLINK 150 BUILDS THEM: parsed, typed, simplified and serialized the way
//! `css_math_expression_node.cc` does (a fingerprinting library s203).
//!
//! Fingerprint's agent sets `a.style.width = "calc( 1px * ( sin( 66911823500 * ( 36781 / -0.55 * cos( -30780.4973...
//! ) ) ) )"` and reads `a.style.width` back. Chrome 150 answers `calc(0.207912px)`: every math function whose
//! arguments are known when the value is parsed is evaluated right then, and what is left is written in Blink's
//! canonical form. This engine echoed the expression (its numbers cut to six digits on the way), so the answer -- and
//! the visitor id it feeds -- was this engine's own. Measured on Chrome 150 (`fixtures/css-answers/calc-probe.js`):
//!
//! ```text
//!   written                              Chrome 150 reads back               this engine read back
//!   calc(1px * sin(0.5))                 calc(0.479426px)                    calc(1px * sin(0.5))
//!   min(1px, 2px)                        calc(1px)                           min(1px, 2px)
//!   calc(min(10%, 20px) + 5px)           calc(5px + min(10%, 20px))          as written
//!   calc(1px * atan2(1, 1))              "" (an angle times a length)        accepted
//!   calc(1px * 1px / 1px)                calc((1px * 1px) * (1 / 1px))       as written
//! ```
//!
//! THE PORT, piece by piece, each a function of the Chromium file named:
//!
//! * the parser (`CSSMathExpressionNodeParser`): `+` and `-` need whitespace on both sides, `(...)` and a nested
//!   `calc()` are a parenthesised sum, `pi`, `e`, `infinity`, `-infinity` and `NaN` are numbers, `none` is a bound of
//!   `clamp()` only, 100 levels at most;
//! * every node's TYPE (`CSSMathType`, typed arithmetic is stable in 150): `1px * 1px` is an intermediate result that
//!   is only valid once divided by a length again, `1px * 45deg` is never a length;
//! * EAGER SIMPLIFICATION at construction (`CreateArithmeticOperationSimplified`, `CreateComparisonFunction`,
//!   `CreateTrigonometricFunction`, ...): an operation whose operands are all numbers or absolute dimensions is
//!   evaluated at once, with Blink's own `SinCosDegrees`, `TanDegrees`, rounding and modulus; a dimension in a non-
//!   canonical absolute unit becomes the canonical one (`1in` is `96px`, `500ms` is `0.5s`, `1turn` `360deg`);
//! * SUM AND PRODUCT simplification (`MaybeSimplifySumOrProductNode`): like terms combined across the whole sum;
//! * SERIALIZATION (`CustomCSSText`, `CollectSumOrProductInOrder`): a number first, then a percentage, then the
//!   dimensions by unit name, then everything else; a negative term subtracts; only `min`/`max`/`clamp`/`round`/
//!   `mod`/`rem`/`hypot`/`abs`/`sign` stand without an outer `calc()` (`IsMathFunction`).
//!
//! What is NOT here: `anchor()`, `anchor-size()`, `calc-size()`, `progress()`, `random()`, `sibling-index()` and the
//! relative colour channels. A caller treats those functions as before; this file answers only for the ones it ports.

use cssparser::{ParseError, Parser, ParserInput, Token};

/// `kMaxExpressionDepth`.
const MAX_DEPTH: u32 = 100;

// ---------------------------------------------------------------------------------------------------------------
// Units
// ---------------------------------------------------------------------------------------------------------------

/// A numeric literal's unit (`CSSPrimitiveValue::UnitType`, the ones a math function takes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Unit {
    Number,
    /// A number token written without a fraction or an exponent -- and, Blink's value pool, any number whose value
    /// is a whole number from 0 to 255 (`CSSNumericLiteralValue::Create`).
    Integer,
    Percent,
    Px,
    Cm,
    Mm,
    Q,
    In,
    Pt,
    Pc,
    Em,
    Rem,
    Ex,
    Rex,
    Ch,
    Rch,
    Ic,
    Ric,
    Lh,
    Rlh,
    Cap,
    Rcap,
    Vw,
    Vh,
    Vi,
    Vb,
    Vmin,
    Vmax,
    Svw,
    Svh,
    Svi,
    Svb,
    Svmin,
    Svmax,
    Lvw,
    Lvh,
    Lvi,
    Lvb,
    Lvmin,
    Lvmax,
    Dvw,
    Dvh,
    Dvi,
    Dvb,
    Dvmin,
    Dvmax,
    Cqw,
    Cqh,
    Cqi,
    Cqb,
    Cqmin,
    Cqmax,
    Deg,
    Rad,
    Grad,
    Turn,
    S,
    Ms,
    Hz,
    Khz,
    Dppx,
    X,
    Dpi,
    Dpcm,
}

/// The dimension units by their text (`CSSPrimitiveValue::UnitTypeToString`).
const DIMENSIONS: &[(&str, Unit)] = &[
    ("px", Unit::Px),
    ("cm", Unit::Cm),
    ("mm", Unit::Mm),
    ("q", Unit::Q),
    ("in", Unit::In),
    ("pt", Unit::Pt),
    ("pc", Unit::Pc),
    ("em", Unit::Em),
    ("rem", Unit::Rem),
    ("ex", Unit::Ex),
    ("rex", Unit::Rex),
    ("ch", Unit::Ch),
    ("rch", Unit::Rch),
    ("ic", Unit::Ic),
    ("ric", Unit::Ric),
    ("lh", Unit::Lh),
    ("rlh", Unit::Rlh),
    ("cap", Unit::Cap),
    ("rcap", Unit::Rcap),
    ("vw", Unit::Vw),
    ("vh", Unit::Vh),
    ("vi", Unit::Vi),
    ("vb", Unit::Vb),
    ("vmin", Unit::Vmin),
    ("vmax", Unit::Vmax),
    ("svw", Unit::Svw),
    ("svh", Unit::Svh),
    ("svi", Unit::Svi),
    ("svb", Unit::Svb),
    ("svmin", Unit::Svmin),
    ("svmax", Unit::Svmax),
    ("lvw", Unit::Lvw),
    ("lvh", Unit::Lvh),
    ("lvi", Unit::Lvi),
    ("lvb", Unit::Lvb),
    ("lvmin", Unit::Lvmin),
    ("lvmax", Unit::Lvmax),
    ("dvw", Unit::Dvw),
    ("dvh", Unit::Dvh),
    ("dvi", Unit::Dvi),
    ("dvb", Unit::Dvb),
    ("dvmin", Unit::Dvmin),
    ("dvmax", Unit::Dvmax),
    ("cqw", Unit::Cqw),
    ("cqh", Unit::Cqh),
    ("cqi", Unit::Cqi),
    ("cqb", Unit::Cqb),
    ("cqmin", Unit::Cqmin),
    ("cqmax", Unit::Cqmax),
    ("deg", Unit::Deg),
    ("rad", Unit::Rad),
    ("grad", Unit::Grad),
    ("turn", Unit::Turn),
    ("s", Unit::S),
    ("ms", Unit::Ms),
    ("hz", Unit::Hz),
    ("khz", Unit::Khz),
    ("dppx", Unit::Dppx),
    ("x", Unit::X),
    ("dpi", Unit::Dpi),
    ("dpcm", Unit::Dpcm),
];

/// `CSSPrimitiveValue::UnitCategory`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitKind {
    Number,
    Percent,
    Length,
    Angle,
    Time,
    Frequency,
    Resolution,
}

impl Unit {
    /// A dimension token's unit, ASCII case-insensitively; `None` for one no math function takes (`fr`, or unknown).
    pub fn dimension(text: &str) -> Option<Unit> {
        DIMENSIONS.iter().find(|(t, _)| t.eq_ignore_ascii_case(text)).map(|(_, u)| *u)
    }

    /// The unit as Blink writes it after a number.
    pub fn text(self) -> &'static str {
        match self {
            Unit::Number | Unit::Integer => "",
            Unit::Percent => "%",
            u => DIMENSIONS.iter().find(|(_, d)| *d == u).map(|(t, _)| *t).unwrap_or(""),
        }
    }

    pub fn kind(self) -> UnitKind {
        use Unit::*;
        match self {
            Number | Integer => UnitKind::Number,
            Percent => UnitKind::Percent,
            Deg | Rad | Grad | Turn => UnitKind::Angle,
            S | Ms => UnitKind::Time,
            Hz | Khz => UnitKind::Frequency,
            Dppx | X | Dpi | Dpcm => UnitKind::Resolution,
            _ => UnitKind::Length,
        }
    }

    /// A length that needs the element, the root or the viewport to become pixels (`IsRelativeUnit`).
    pub fn is_relative(self) -> bool {
        self.kind() == UnitKind::Length && !matches!(self, Unit::Px | Unit::Cm | Unit::Mm | Unit::Q | Unit::In | Unit::Pt | Unit::Pc)
    }

    /// `ConversionToCanonicalUnitsScaleFactor`.
    pub fn factor(self) -> f64 {
        const PX_PER_CM: f64 = 96.0 / 2.54;
        match self {
            Unit::Ms => 0.001,
            Unit::Cm => PX_PER_CM,
            Unit::Dpcm => 1.0 / PX_PER_CM,
            Unit::Mm => PX_PER_CM / 10.0,
            Unit::Q => PX_PER_CM / 40.0,
            Unit::In => 96.0,
            Unit::Dpi => 1.0 / 96.0,
            Unit::Pt => 96.0 / 72.0,
            Unit::Pc => 96.0 / 6.0,
            Unit::Rad => 180.0 / std::f64::consts::PI,
            Unit::Grad => 0.9,
            Unit::Turn => 360.0,
            Unit::Khz => 1000.0,
            _ => 1.0,
        }
    }

    /// `CSSPrimitiveValue::CanonicalUnit`: `None` for a percentage, which has none.
    pub fn canonical(self) -> Option<Unit> {
        Some(match self.kind() {
            UnitKind::Number => Unit::Number,
            UnitKind::Percent => return None,
            UnitKind::Length => Unit::Px,
            UnitKind::Angle => Unit::Deg,
            UnitKind::Time => Unit::S,
            UnitKind::Frequency => Unit::Hz,
            UnitKind::Resolution => Unit::Dppx,
        })
    }

    /// `CSSPrimitiveValue::UnitTypeToUnitCategory`, what two literals must share to be added in one unit: a RELATIVE
    /// length is `kUOther` there, so `10px + 1em` stays a sum.
    fn combine_kind(self) -> Option<UnitKind> {
        if self.is_relative() {
            None
        } else {
            Some(self.kind())
        }
    }

    /// `ResolvedUnitTypeForSimplification`: an integer is a number.
    fn simple(self) -> Unit {
        if self == Unit::Integer {
            Unit::Number
        } else {
            self
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Categories and types
// ---------------------------------------------------------------------------------------------------------------

/// `CalculationResultCategory`, in Blink's order (the add/subtract table is indexed by it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cat {
    Number = 0,
    Length,
    Percent,
    /// A length that needs layout: a length mixed with a percentage, `kCalcLengthFunction`.
    LengthFunction,
    /// `1px * 1px`: valid only on the way to another type.
    Intermediate,
    Angle,
    Time,
    Frequency,
    Resolution,
    Ident,
    PercentAngle,
    Other,
}

use Cat as C;

/// `kAddSubtractResult`.
const ADD_SUB: [[Cat; 11]; 11] = [
    [C::Number, C::Other, C::Other, C::Other, C::Other, C::Other, C::Other, C::Other, C::Other, C::Other, C::Other],
    [C::Other, C::Length, C::LengthFunction, C::LengthFunction, C::Other, C::Other, C::Other, C::Other, C::Other, C::Other, C::Other],
    [C::Other, C::LengthFunction, C::Percent, C::LengthFunction, C::Other, C::PercentAngle, C::Other, C::Other, C::Other, C::Other, C::PercentAngle],
    [C::Other, C::LengthFunction, C::LengthFunction, C::LengthFunction, C::Other, C::Other, C::Other, C::Other, C::Other, C::Other, C::Other],
    [C::Other, C::Other, C::Other, C::Other, C::Other, C::Other, C::Other, C::Other, C::Other, C::Other, C::Other],
    [C::Other, C::Other, C::PercentAngle, C::Other, C::Other, C::Angle, C::Other, C::Other, C::Other, C::Other, C::PercentAngle],
    [C::Other, C::Other, C::Other, C::Other, C::Other, C::Other, C::Time, C::Other, C::Other, C::Other, C::Other],
    [C::Other, C::Other, C::Other, C::Other, C::Other, C::Other, C::Other, C::Frequency, C::Other, C::Other, C::Other],
    [C::Other, C::Other, C::Other, C::Other, C::Other, C::Other, C::Other, C::Other, C::Resolution, C::Other, C::Other],
    [C::Other, C::Other, C::Other, C::Other, C::Other, C::Other, C::Other, C::Other, C::Other, C::Other, C::Other],
    [C::Other, C::Other, C::PercentAngle, C::Other, C::Other, C::PercentAngle, C::Other, C::Other, C::Other, C::Other, C::PercentAngle],
];

fn add_sub(a: Cat, b: Cat) -> Cat {
    if a == C::Other || b == C::Other {
        return C::Other;
    }
    ADD_SUB[a as usize][b as usize]
}

fn unit_cat(u: Unit) -> Cat {
    match u.kind() {
        UnitKind::Number => C::Number,
        UnitKind::Percent => C::Percent,
        UnitKind::Length => C::Length,
        UnitKind::Angle => C::Angle,
        UnitKind::Time => C::Time,
        UnitKind::Frequency => C::Frequency,
        UnitKind::Resolution => C::Resolution,
    }
}

// `CSSMathType::BaseType`: percent last, which `Category` relies on.
const LENGTH: usize = 0;
const ANGLE: usize = 1;
const TIME: usize = 2;
const FREQUENCY: usize = 3;
const RESOLUTION: usize = 4;
const PERCENT: usize = 6;
const BASE_TYPES: usize = 7;

/// `CSSMathType` -- css-typed-om's numeric type: a power per base type and a percent hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MathType {
    powers: [i8; BASE_TYPES],
    hint: Option<usize>,
    valid: bool,
}

impl Default for MathType {
    fn default() -> Self {
        MathType { powers: [0; BASE_TYPES], hint: None, valid: true }
    }
}

impl MathType {
    const INVALID: MathType = MathType { powers: [0; BASE_TYPES], hint: None, valid: false };

    fn of_cat(cat: Cat) -> MathType {
        let mut t = MathType::default();
        let base = match cat {
            C::Number => return t,
            C::Length | C::LengthFunction => LENGTH,
            C::Percent => PERCENT,
            C::Angle | C::PercentAngle => ANGLE,
            C::Time => TIME,
            C::Frequency => FREQUENCY,
            C::Resolution => RESOLUTION,
            C::Intermediate | C::Ident | C::Other => return MathType::INVALID,
        };
        t.powers[base] = 1;
        if cat == C::LengthFunction {
            t.hint = Some(LENGTH);
        } else if cat == C::PercentAngle {
            t.hint = Some(ANGLE);
        }
        t
    }

    /// `CSSMathType(node)`: an arithmetic operation carries its own type, anything else its category's.
    fn of(e: &Expr) -> MathType {
        match &e.node {
            Node::Op(op, _, ty, _) if op.is_arithmetic() => *ty,
            _ => MathType::of_cat(e.cat),
        }
    }

    fn category(&self) -> Cat {
        if !self.valid {
            return C::Other;
        }
        let mut ty = None;
        let mut sum: i16 = 0;
        for (i, &p) in self.powers.iter().enumerate() {
            if p == 0 {
                continue;
            }
            if sum != 0 {
                if i != PERCENT {
                    return C::Intermediate;
                }
                sum += p as i16;
                break;
            }
            ty = Some(i);
            sum += p as i16;
        }
        if sum == 0 {
            return C::Number;
        }
        if sum != 1 {
            return C::Intermediate;
        }
        if self.hint.is_some() {
            return if self.powers[LENGTH] != 0 {
                C::LengthFunction
            } else if self.powers[ANGLE] != 0 {
                C::PercentAngle
            } else {
                C::Other
            };
        }
        match ty {
            Some(LENGTH) => C::Length,
            Some(PERCENT) => C::Percent,
            Some(ANGLE) => C::Angle,
            Some(TIME) => C::Time,
            Some(FREQUENCY) => C::Frequency,
            Some(RESOLUTION) => C::Resolution,
            _ => C::Other,
        }
    }

    fn is_intermediate(&self) -> bool {
        self.valid && self.category() == C::Intermediate
    }

    fn apply_hint(&mut self, hint: usize) {
        if self.hint.is_some() {
            return;
        }
        self.hint = Some(hint);
        if hint != PERCENT {
            self.powers[hint] += self.powers[PERCENT];
            self.powers[PERCENT] = 0;
        }
    }

    /// Both operands take a hint either one carries; two different hints are a failure.
    fn hints(t1: &mut MathType, t2: &mut MathType) -> bool {
        if t1.hint.is_some() && t2.hint.is_some() && t1.hint != t2.hint {
            return false;
        }
        if let (Some(h), None) = (t1.hint, t2.hint) {
            t2.apply_hint(h);
        }
        if let (None, Some(h)) = (t1.hint, t2.hint) {
            t1.apply_hint(h);
        }
        true
    }

    /// css-typed-om "add two types".
    fn add(mut t1: MathType, mut t2: MathType) -> MathType {
        if !MathType::hints(&mut t1, &mut t2) {
            return MathType::INVALID;
        }
        if t1 == t2 {
            return t1;
        }
        let sum = |t: &MathType| t.powers.iter().map(|&p| p as i32).sum::<i32>();
        let has_percent = t1.powers[PERCENT] != 0 || t2.powers[PERCENT] != 0;
        let other1 = t1.powers[PERCENT] as i32 != sum(&t1);
        let other2 = t2.powers[PERCENT] as i32 != sum(&t2);
        if has_percent && (other1 || other2) {
            for hint in 0..BASE_TYPES {
                if hint == PERCENT {
                    continue;
                }
                let (mut a, mut b) = (t1, t2);
                a.apply_hint(hint);
                b.apply_hint(hint);
                if a == b {
                    return a;
                }
            }
        }
        MathType::INVALID
    }

    /// css-typed-om "multiply two types".
    fn mul(mut t1: MathType, mut t2: MathType) -> MathType {
        if !MathType::hints(&mut t1, &mut t2) {
            return MathType::INVALID;
        }
        for i in 0..BASE_TYPES {
            t1.powers[i] += t2.powers[i];
        }
        t1
    }

    fn neg(mut self) -> MathType {
        for p in self.powers.iter_mut() {
            *p = -*p;
        }
        self
    }
}

/// `DetermineType`.
fn determine_type(l: &Expr, r: &Expr, op: Op) -> MathType {
    let (a, b) = (MathType::of(l), MathType::of(r));
    if !a.valid || !b.valid {
        return MathType::INVALID;
    }
    match op {
        Op::Add | Op::Sub => MathType::add(a, b),
        Op::Mul => MathType::mul(a, b),
        Op::Div => MathType::mul(a, b.neg()),
        _ => MathType::INVALID,
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The tree
// ---------------------------------------------------------------------------------------------------------------

/// `CSSMathOperator` (a division is stored as a product with an inverse, as Blink stores it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Min,
    Max,
    Clamp,
    RoundNearest,
    RoundUp,
    RoundDown,
    RoundToZero,
    Mod,
    Rem,
    Hypot,
    Abs,
    Sign,
    Log,
    Exp,
    Sqrt,
    Pow,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Atan2,
    Invert,
}

impl Op {
    fn is_arithmetic(self) -> bool {
        matches!(self, Op::Add | Op::Sub | Op::Mul | Op::Div | Op::Invert)
    }

    fn is_add_sub(self) -> bool {
        matches!(self, Op::Add | Op::Sub)
    }

    fn is_mul_div(self) -> bool {
        matches!(self, Op::Mul | Op::Div)
    }

    fn is_round(self) -> bool {
        matches!(self, Op::RoundNearest | Op::RoundUp | Op::RoundDown | Op::RoundToZero)
    }

    /// `CSSMathExpressionOperation::IsMathFunction` -- the ones that serialize without a `calc()` around them.
    /// (`IsTrigonometricFunction` is Blink's name for `hypot`, and only for it.)
    fn stands_alone(self) -> bool {
        matches!(self, Op::Min | Op::Max | Op::Clamp | Op::Mod | Op::Rem | Op::Hypot | Op::Abs | Op::Sign) || self.is_round()
    }

    fn text(self) -> &'static str {
        match self {
            Op::Add => "+",
            Op::Sub => "-",
            Op::Mul => "*",
            Op::Div => "/",
            Op::Min => "min",
            Op::Max => "max",
            Op::Clamp => "clamp",
            Op::RoundNearest | Op::RoundUp | Op::RoundDown | Op::RoundToZero => "round",
            Op::Mod => "mod",
            Op::Rem => "rem",
            Op::Hypot => "hypot",
            Op::Abs => "abs",
            Op::Sign => "sign",
            Op::Log => "log",
            Op::Exp => "exp",
            Op::Sqrt => "sqrt",
            Op::Pow => "pow",
            Op::Sin => "sin",
            Op::Cos => "cos",
            Op::Tan => "tan",
            Op::Asin => "asin",
            Op::Acos => "acos",
            Op::Atan => "atan",
            Op::Atan2 => "atan2",
            Op::Invert => "",
        }
    }
}

/// One node of a calculation tree (`CSSMathExpressionNode`).
#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    node: Node,
    cat: Cat,
    /// `IsNestedCalc`: the node stood in parentheses, which decides how a subtraction distributes over it.
    nested: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum Node {
    /// A numeric literal.
    Lit(f64, Unit),
    /// `none`, a bound of `clamp()`.
    NoneKw,
    /// An operation: its operator, operands, its type (meaningful for arithmetic) and whether an intermediate type is
    /// anywhere in it (`HasNestedIntermediateResult`).
    Op(Op, Vec<Expr>, MathType, bool),
}

/// The unit a number literal of `value` ends up with: Blink's value pool hands out an INTEGER for a whole number in
/// 0..=255 (`CSSNumericLiteralValue::Create`), which is why `(grid: 0.0)` is a valid `<integer>` query.
pub fn pooled_unit(value: f64, unit: Unit) -> Unit {
    if matches!(unit, Unit::Number | Unit::Integer)
        && (0.0..=255.0).contains(&value)
        && value.trunc() == value
        && !(value == 0.0 && value.is_sign_negative())
    {
        return Unit::Integer;
    }
    unit
}

impl Expr {
    /// A numeric literal (`CSSMathExpressionNumericLiteral`): an absolute dimension in its canonical unit at once.
    pub fn literal(value: f64, unit: Unit) -> Expr {
        let unit = pooled_unit(value, unit);
        let cat = unit_cat(unit);
        let simplify = match unit.kind() {
            UnitKind::Number | UnitKind::Percent => false,
            UnitKind::Length => !unit.is_relative(),
            _ => true,
        };
        let (value, unit) = match unit.canonical() {
            Some(c) if simplify => (value * unit.factor(), c),
            _ => (value, unit),
        };
        Expr { node: Node::Lit(value, unit), cat, nested: false }
    }

    fn op(op: Op, args: Vec<Expr>, cat: Cat, ty: MathType) -> Expr {
        let mut intermediate = ty.is_intermediate();
        if op.is_arithmetic() {
            intermediate |= args.first().is_some_and(Expr::nested_intermediate) || args.last().is_some_and(Expr::nested_intermediate);
        }
        Expr { node: Node::Op(op, args, ty, intermediate), cat, nested: false }
    }

    /// What the expression is: a number, a length, a length that needs layout, ...
    pub fn category(&self) -> Cat {
        self.cat
    }

    /// The value and unit when the expression simplified to one literal.
    pub fn as_literal(&self) -> Option<(f64, Unit)> {
        match self.node {
            Node::Lit(v, u) => Some((v, u.simple())),
            _ => None,
        }
    }

    fn lit(&self) -> Option<(f64, Unit)> {
        match self.node {
            Node::Lit(v, u) => Some((v, u)),
            _ => None,
        }
    }

    fn is_literal(&self) -> bool {
        matches!(self.node, Node::Lit(..))
    }

    fn is_op(&self) -> bool {
        matches!(self.node, Node::Op(..))
    }

    fn op_kind(&self) -> Option<Op> {
        match &self.node {
            Node::Op(op, ..) => Some(*op),
            _ => None,
        }
    }

    fn args(&self) -> &[Expr] {
        match &self.node {
            Node::Op(_, args, ..) => args.as_slice(),
            _ => &[],
        }
    }

    fn nested_intermediate(&self) -> bool {
        matches!(self.node, Node::Op(_, _, _, true))
    }

    fn is_none_kw(&self) -> bool {
        matches!(self.node, Node::NoneKw)
    }

    /// `InvolvesLayout`: a percentage, or a length that needs one.
    fn involves_layout(&self) -> bool {
        if matches!(self.cat, C::Percent | C::LengthFunction) {
            return true;
        }
        self.args().iter().any(Expr::involves_layout)
    }

    /// Does any percentage take part (a layout basis is needed to evaluate it)?
    pub fn has_percent(&self) -> bool {
        self.involves_layout()
    }

    /// `ResolvedUnitType`, `None` for `kUnknown`.
    fn resolved_unit(&self) -> Option<Unit> {
        match &self.node {
            Node::Lit(_, u) => Some(*u),
            Node::NoneKw => None,
            Node::Op(op, args, ..) => match self.cat {
                C::Number => Some(Unit::Number),
                C::Angle | C::Time | C::Frequency | C::Length | C::Percent | C::Resolution => match op {
                    Op::Mul => {
                        if args[0].cat == C::Number {
                            args[1].resolved_unit()
                        } else if args[1].cat == C::Number {
                            args[0].resolved_unit()
                        } else {
                            None
                        }
                    }
                    Op::Add | Op::Sub | Op::Min | Op::Max | Op::Clamp | Op::Sqrt | Op::RoundNearest | Op::RoundUp
                    | Op::RoundDown | Op::RoundToZero | Op::Mod | Op::Rem | Op::Hypot | Op::Abs | Op::Invert => {
                        let first = args[0].resolved_unit()?;
                        args[1..].iter().all(|a| a.resolved_unit() == Some(first)).then_some(first)
                    }
                    Op::Sign | Op::Pow | Op::Sin | Op::Cos | Op::Log | Op::Exp | Op::Tan => Some(Unit::Number),
                    Op::Asin | Op::Acos | Op::Atan | Op::Atan2 => Some(Unit::Deg),
                    Op::Div => None,
                },
                _ => None,
            },
        }
    }

    fn simple_unit(&self) -> Option<Unit> {
        self.resolved_unit().map(Unit::simple)
    }

    /// `ComputeValueInCanonicalUnit()` -- with no element to resolve relative units against.
    fn canonical_value(&self) -> Option<f64> {
        match &self.node {
            Node::Lit(v, u) => match u.kind() {
                UnitKind::Number | UnitKind::Percent => Some(*v),
                UnitKind::Length if u.is_relative() => None,
                _ => Some(v * u.factor()),
            },
            Node::NoneKw => None,
            Node::Op(op, args, ..) => {
                let canonical = matches!(self.cat, C::Number | C::Length | C::Percent | C::Angle | C::Time | C::Frequency | C::Resolution);
                if self.cat != C::Intermediate && !canonical {
                    return None;
                }
                let values: Option<Vec<f64>> = args.iter().map(Expr::canonical_value).collect();
                Some(evaluate(&values?, *op))
            }
        }
    }

    /// `CanEagerlySimplify(node)`.
    fn can_eager(&self) -> bool {
        if self.involves_layout() || self.is_op() {
            return false;
        }
        match self.cat {
            C::Number | C::Angle | C::Time | C::Frequency | C::Resolution => self.canonical_value().is_some(),
            C::Length => self.lit().is_some_and(|(_, u)| !u.is_relative()),
            _ => false,
        }
    }

    /// A literal's value (`DoubleValue`).
    fn double(&self) -> f64 {
        self.lit().map_or(0.0, |(v, _)| v)
    }

    /// A numeric literal of a unit with a double value (`IsNumericNodeWithDoubleValue`): every literal here.
    fn numeric(&self) -> Option<(f64, Unit)> {
        self.lit().map(|(v, u)| (v, u.simple()))
    }
}

fn all_eager(args: &[Expr]) -> bool {
    args.iter().all(Expr::can_eager)
}

// ---------------------------------------------------------------------------------------------------------------
// Evaluation (`EvaluateOperator`, `math_functions.h`, `sin_cos_degrees.h`)
// ---------------------------------------------------------------------------------------------------------------

/// WTF's `Rad2deg`.
fn rad2deg(r: f64) -> f64 {
    r * (180.0 / std::f64::consts::PI)
}

/// `gfx::SinCosDegrees`: range-reduced to [0, 45] degrees, exact at the multiples of 90.
fn sin_cos_degrees(degrees: f64) -> (f64, f64) {
    const H: f64 = std::f64::consts::SQRT_2 / 2.0;
    const TABLE: [(f64, f64); 8] = [(0.0, 1.0), (H, H), (1.0, 0.0), (H, -H), (0.0, -1.0), (-H, -H), (-1.0, 0.0), (-H, H)];
    if degrees > -90000000.0 && degrees < 90000000.0 {
        let n45 = degrees / 45.0;
        let mut octant = n45 as i32;
        if octant as f64 == n45 {
            return TABLE[(octant & 7) as usize];
        }
        if degrees < 0.0 {
            octant -= 1;
        }
        let mut d = degrees - octant as f64 * 45.0;
        if octant & 1 != 0 {
            d = 45.0 - d;
        }
        // base::DegToRad: `deg * pi / 180`, in that order.
        let rad = d * std::f64::consts::PI / 180.0;
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
    let d = degrees % 360.0;
    let rad = d * std::f64::consts::PI / 180.0;
    (rad.sin(), rad.cos())
}

/// Blink's `TanDegrees`.
fn tan_degrees(degrees: f64) -> f64 {
    if degrees > -90000000.0 && degrees < 90000000.0 {
        let n45 = degrees / 45.0;
        let octant = n45 as i32;
        if octant as f64 == n45 {
            const TABLE: [f64; 8] = [0.0, 1.0, f64::INFINITY, -1.0, 0.0, 1.0, f64::NEG_INFINITY, -1.0];
            return TABLE[(octant & 7) as usize];
        }
    }
    // WTF's `Deg2rad`.
    (degrees * (std::f64::consts::PI / 180.0)).tan()
}

fn trig(op: Op, a: f64, b: f64) -> f64 {
    match op {
        Op::Sin => sin_cos_degrees(a).0,
        Op::Cos => sin_cos_degrees(a).1,
        Op::Tan => tan_degrees(a),
        Op::Asin => rad2deg(a.asin()),
        Op::Acos => rad2deg(a.acos()),
        Op::Atan => rad2deg(a.atan()),
        Op::Atan2 => rad2deg(a.atan2(b)),
        _ => f64::NAN,
    }
}

/// `GetNearestMultiples`.
fn nearest_multiples(a: f64, b: f64) -> (f64, f64) {
    let negative = a < 0.0;
    let (mut a, mut b) = (a.abs(), b.abs());
    let mut c = -(a % b);
    let mut lower = a + c;
    if a.abs() > c.abs() {
        std::mem::swap(&mut a, &mut c);
    }
    if a.abs() > b.abs() {
        std::mem::swap(&mut a, &mut b);
    }
    if b.abs() > c.abs() {
        std::mem::swap(&mut b, &mut c);
    }
    let mut upper = a + b + c;
    if negative {
        std::mem::swap(&mut lower, &mut upper);
        lower = -lower;
        upper = -upper;
    }
    (lower, upper)
}

fn signed_zero(negative: bool) -> f64 {
    if negative {
        -0.0
    } else {
        0.0
    }
}

/// `EvaluateSteppedValueFunction`: `round()`, `mod()`, `rem()`.
fn stepped(op: Op, a: f64, b: f64) -> f64 {
    if b == 0.0 || (a.is_infinite() && b.is_infinite()) {
        return f64::NAN;
    }
    if op.is_round() && (a % b == 0.0 || (a.is_infinite() && !b.is_infinite())) {
        return a;
    }
    if matches!(op, Op::Mod | Op::Rem) && a.is_infinite() {
        return f64::NAN;
    }
    let (mut lower, mut upper) = nearest_multiples(a, b);
    match op {
        Op::RoundNearest => {
            if !a.is_infinite() && b.is_infinite() {
                return signed_zero(a.is_sign_negative());
            }
            let negative = a < 0.0;
            if negative {
                std::mem::swap(&mut lower, &mut upper);
            }
            let distance = (a % b).abs();
            let half = b.abs() / 2.0;
            if distance < half || (negative && distance == half) {
                lower
            } else {
                upper
            }
        }
        Op::RoundUp => {
            if !a.is_infinite() && b.is_infinite() {
                if a == 0.0 {
                    a
                } else if a.is_sign_negative() {
                    -0.0
                } else {
                    f64::INFINITY
                }
            } else {
                upper
            }
        }
        Op::RoundDown => {
            if !a.is_infinite() && b.is_infinite() {
                if a == 0.0 {
                    a
                } else if a.is_sign_negative() {
                    f64::NEG_INFINITY
                } else {
                    0.0
                }
            } else {
                lower
            }
        }
        Op::RoundToZero => {
            if !a.is_infinite() && b.is_infinite() {
                signed_zero(a.is_sign_negative())
            } else if upper.abs() < lower.abs() {
                upper
            } else {
                lower
            }
        }
        Op::Mod => {
            if b.is_infinite() && a.is_sign_negative() != b.is_sign_negative() {
                return f64::NAN;
            }
            let r = a % b;
            if r == 0.0 {
                return 0.0f64.copysign(b);
            }
            if r.is_sign_negative() != b.is_sign_negative() {
                return r + b;
            }
            r
        }
        Op::Rem => a % b,
        _ => f64::NAN,
    }
}

/// `EvaluateSignFunction`.
fn sign(v: f64) -> f64 {
    if v == 0.0 || v.is_nan() {
        v
    } else if v > 0.0 {
        1.0
    } else {
        -1.0
    }
}

/// `CSSMathExpressionOperation::EvaluateOperator`.
fn evaluate(v: &[f64], op: Op) -> f64 {
    if let Some(nan) = v.iter().find(|x| x.is_nan()) {
        return *nan;
    }
    let at = |i: usize| v.get(i).copied().unwrap_or(f64::NAN);
    match op {
        Op::Add => at(0) + at(1),
        Op::Sub => at(0) - at(1),
        Op::Mul => at(0) * at(1),
        Op::Div => at(0) / at(1),
        Op::Invert => 1.0 / at(0),
        Op::Min => {
            let Some(&first) = v.first() else { return f64::NAN };
            let mut m = first;
            for &x in v {
                if m == 0.0 && x == 0.0 && m.is_sign_negative() != x.is_sign_negative() {
                    m = -0.0;
                    continue;
                }
                m = m.min(x);
            }
            m
        }
        Op::Max => {
            let Some(&first) = v.first() else { return f64::NAN };
            let mut m = first;
            for &x in v {
                if m == 0.0 && x == 0.0 && m.is_sign_negative() != x.is_sign_negative() {
                    m = 0.0;
                    continue;
                }
                m = m.max(x);
            }
            m
        }
        Op::Clamp => {
            let (min, val, max) = (at(0), at(1), at(2));
            let mut minimum = val.min(max);
            if val == 0.0 && max == 0.0 && !val.is_sign_negative() && max.is_sign_negative() {
                minimum = -0.0;
            }
            let mut maximum = min.max(minimum);
            if min == 0.0 && minimum == 0.0 && min.is_sign_negative() && !minimum.is_sign_negative() {
                maximum = 0.0;
            }
            maximum
        }
        Op::RoundNearest | Op::RoundUp | Op::RoundDown | Op::RoundToZero | Op::Mod | Op::Rem => stepped(op, at(0), at(1)),
        Op::Exp => at(0).exp(),
        Op::Log => {
            if v.len() == 2 {
                at(0).log2() / at(1).log2()
            } else {
                at(0).ln()
            }
        }
        Op::Sqrt => at(0).sqrt(),
        Op::Hypot => v.iter().fold(0.0f64, |acc, &x| acc.hypot(x)),
        Op::Abs => at(0).abs(),
        Op::Sign => sign(at(0)),
        Op::Pow => at(0).powf(at(1)),
        Op::Sin | Op::Cos | Op::Tan | Op::Asin | Op::Acos | Op::Atan => trig(op, at(0), 0.0),
        Op::Atan2 => trig(op, at(0), at(1)),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Construction with simplification
// ---------------------------------------------------------------------------------------------------------------

/// `CreateArithmeticOperation` (typed arithmetic): a division becomes a product with an inverse.
fn create_arith(l: Expr, r: Expr, op: Op) -> Option<Expr> {
    let ty = determine_type(&l, &r, op);
    if !ty.valid {
        return None;
    }
    let cat = ty.category();
    if cat == C::Other {
        return None;
    }
    if op == Op::Div {
        let inverted = create_invert(r)?;
        if l.cat == C::Number && l.is_literal() && l.double() == 1.0 {
            return Some(inverted);
        }
        return Some(Expr::op(Op::Mul, vec![l, inverted], cat, ty));
    }
    Some(Expr::op(op, vec![l, r], cat, ty))
}

/// `CreateInvertFunction`.
fn create_invert(e: Expr) -> Option<Expr> {
    if !e.is_op() && e.cat == C::Number {
        if let Some(v) = e.canonical_value().filter(|v| *v != 0.0) {
            return Some(Expr::literal(1.0 / v, Unit::Number));
        }
    }
    if e.op_kind() == Some(Op::Invert) {
        return e.args().first().cloned();
    }
    if e.cat != C::Number {
        let ty = MathType::of(&e).neg();
        return Some(Expr::op(Op::Invert, vec![e], C::Intermediate, ty));
    }
    Some(Expr::op(Op::Invert, vec![e], C::Number, MathType::default()))
}

/// `CanArithmeticOperationBeSimplified`.
fn can_simplify(l: &Expr, r: &Expr, op: Op) -> bool {
    if !l.is_literal() || !r.is_literal() {
        return false;
    }
    if op.is_mul_div() && l.cat != C::Number && r.cat != C::Number {
        return false;
    }
    !determine_type(l, r, op).is_intermediate()
}

/// `MaybeDistributeArithmeticOperation`: `(a + b) * n` and `n * (a + b)` over literals. `Some(None)` is a
/// distribution that failed.
fn distribute(l: &Expr, r: &Expr, op: Op) -> Option<Option<Expr>> {
    if !op.is_mul_div() || l.cat == C::Intermediate || r.cat == C::Intermediate {
        return None;
    }
    let sum_of_literals = |e: &Expr| e.op_kind().is_some_and(Op::is_add_sub) && e.args().iter().all(Expr::is_literal);
    if sum_of_literals(l) && r.is_literal() && r.cat == C::Number {
        let inner = l.op_kind()?;
        let result = (|| {
            let a = arith(l.args()[0].clone(), r.clone(), op)?;
            let b = arith(l.args()[1].clone(), r.clone(), op)?;
            let mut out = arith(a, b, inner)?;
            out.nested = true;
            Some(out)
        })();
        return Some(result);
    }
    if sum_of_literals(r) && l.is_literal() && l.cat == C::Number && op != Op::Div {
        let inner = r.op_kind()?;
        let result = (|| {
            let a = arith(l.clone(), r.args()[0].clone(), op)?;
            let b = arith(l.clone(), r.args()[1].clone(), op)?;
            let mut out = arith(a, b, inner)?;
            out.nested = true;
            Some(out)
        })();
        return Some(result);
    }
    None
}

/// `CreateArithmeticOperationSimplified`.
fn arith(l: Expr, r: Expr, op: Op) -> Option<Expr> {
    if l.is_none_kw() || r.is_none_kw() {
        return None;
    }
    if let Some(distributed) = distribute(&l, &r, op) {
        return distributed;
    }
    if !can_simplify(&l, &r, op) {
        return create_arith(l, r, op);
    }
    let (lc, rc) = (l.cat, r.cat);
    if lc == C::Number && rc == C::Number {
        return Some(Expr::literal(evaluate(&[l.double(), r.double()], op), Unit::Number));
    }
    if op.is_add_sub() {
        if lc == rc {
            if let (Some(lt), Some(rt)) = (l.simple_unit(), r.simple_unit()) {
                if lt == rt {
                    return Some(Expr::literal(evaluate(&[l.double(), r.double()], op), lt));
                }
                if lt.combine_kind().is_some() && lt.combine_kind() == rt.combine_kind() {
                    if let Some(canonical) = lt.canonical() {
                        let (a, b) = (l.double() * lt.factor(), r.double() * rt.factor());
                        return Some(Expr::literal(evaluate(&[a, b], op), canonical));
                    }
                }
            }
        }
    } else {
        if rc != C::Number && op == Op::Div {
            return None;
        }
        let (number, other) = if lc == C::Number {
            (&l, &r)
        } else if rc == C::Number {
            (&r, &l)
        } else {
            return create_arith(l, r, op);
        };
        if let Some(ot) = other.simple_unit() {
            return Some(Expr::literal(evaluate(&[other.double(), number.double()], op), ot));
        }
    }
    create_arith(l, r, op)
}

/// `CreateComparisonFunction`: `min()`, `max()`, `clamp()`.
fn comparison(args: Vec<Expr>, op: Op) -> Option<Expr> {
    if op == Op::Clamp && args.len() == 3 {
        let (min, val, max) = (&args[0], &args[1], &args[2]);
        if val.is_none_kw() {
            return None;
        }
        if min.is_none_kw() && max.is_none_kw() {
            return Some(val.clone());
        }
        if min.is_none_kw() {
            return comparison(vec![val.clone(), max.clone()], Op::Min);
        }
        if max.is_none_kw() {
            return comparison(vec![min.clone(), val.clone()], Op::Max);
        }
    }
    let cat = comparison_cat(&args);
    if cat == C::Other {
        return None;
    }
    if all_eager(&args) {
        let values: Option<Vec<f64>> = args.iter().map(Expr::canonical_value).collect();
        let unit = args[0].resolved_unit().and_then(Unit::canonical)?;
        return Some(Expr::literal(evaluate(&values?, op), unit));
    }
    if args.len() == 1 {
        return args.into_iter().next();
    }
    Some(Expr::op(op, args, cat, MathType::default()))
}

/// `DetermineComparisonCategory`.
fn comparison_cat(args: &[Expr]) -> Cat {
    let mut cat = C::Other;
    for (i, a) in args.iter().enumerate() {
        cat = if i == 0 { a.cat } else { add_sub(cat, a.cat) };
        if cat == C::Other {
            break;
        }
    }
    cat
}

/// `CreateTrigonometricFunction`.
fn trigonometric(args: Vec<Expr>, op: Op) -> Option<Expr> {
    let number_out = matches!(op, Op::Sin | Op::Cos | Op::Tan);
    if args.len() == 1 {
        let c = args[0].cat;
        let ok = if number_out { matches!(c, C::Number | C::Angle) } else { c == C::Number };
        if !ok {
            return None;
        }
    } else if args[0].cat != args[args.len() - 1].cat {
        return None;
    }
    if !all_eager(&args) {
        let cat = if number_out { C::Number } else { C::Angle };
        return Some(Expr::op(op, args, cat, MathType::default()));
    }
    let mut a = args[0].canonical_value()?;
    if number_out && args[0].cat == C::Number {
        a = rad2deg(a);
    }
    let b = if op == Op::Atan2 { args[args.len() - 1].canonical_value()? } else { 0.0 };
    Some(Expr::literal(trig(op, a, b), if number_out { Unit::Number } else { Unit::Deg }))
}

/// `CreateSteppedValueFunction`: `round()`, `mod()`, `rem()` of two operands.
fn stepped_fn(args: Vec<Expr>, op: Op) -> Option<Expr> {
    if args.len() != 2 {
        return None;
    }
    let cat = add_sub(args[0].cat, args[1].cat);
    if cat == C::Other {
        return None;
    }
    if all_eager(&args) {
        let (a, b) = (args[0].canonical_value()?, args[1].canonical_value()?);
        let unit = args[0].resolved_unit().and_then(Unit::canonical)?;
        return Some(Expr::literal(stepped(op, a, b), unit));
    }
    Some(Expr::op(op, args, cat, MathType::default()))
}

/// `CreateExponentialFunction`: `pow()`, `sqrt()`, `hypot()`, `log()`, `exp()`.
fn exponential(args: Vec<Expr>, op: Op) -> Option<Expr> {
    if args.first()?.cat == C::Intermediate {
        return None;
    }
    let number = |e: &Expr| (e.cat == C::Number).then(|| e.double());
    let (value, unit) = match op {
        Op::Pow => {
            let cat = comparison_cat(&args);
            if cat != C::Number {
                return None;
            }
            if !all_eager(&args) {
                return Some(Expr::op(op, args, cat, MathType::default()));
            }
            (args[0].canonical_value()?.powf(args[1].canonical_value()?), Unit::Number)
        }
        Op::Sqrt => {
            if !args[0].can_eager() {
                let cat = args[0].cat;
                return Some(Expr::op(op, args, cat, MathType::default()));
            }
            (number(&args[0])?.sqrt(), Unit::Number)
        }
        Op::Hypot => {
            let cat = comparison_cat(&args);
            if cat == C::Other {
                return None;
            }
            if !all_eager(&args) {
                return Some(Expr::op(op, args, cat, MathType::default()));
            }
            let mut v = 0.0f64;
            for a in &args {
                v = v.hypot(a.canonical_value()?);
            }
            (v, args[0].resolved_unit().and_then(Unit::canonical)?)
        }
        Op::Log => {
            if args[0].cat != C::Number || args[args.len() - 1].cat != C::Number {
                return None;
            }
            if !all_eager(&args) {
                return Some(Expr::op(op, args, C::Number, MathType::default()));
            }
            let a = number(&args[0])?;
            let v = if args.len() == 2 { a.log2() / number(&args[1])?.log2() } else { a.ln() };
            (v, Unit::Number)
        }
        Op::Exp => {
            if !args[0].can_eager() {
                return Some(Expr::op(op, args, C::Number, MathType::default()));
            }
            (number(&args[0])?.exp(), Unit::Number)
        }
        _ => return None,
    };
    Some(Expr::literal(value, unit))
}

/// `CreateSignRelatedFunction`: `abs()`, `sign()`.
fn sign_related(args: Vec<Expr>, op: Op) -> Option<Expr> {
    let a = args.first()?;
    if MathType::of(a).is_intermediate() {
        return None;
    }
    if a.can_eager() {
        let v = a.canonical_value()?;
        return Some(match op {
            Op::Abs => Expr::literal(v.abs(), a.resolved_unit()?),
            _ => Expr::literal(sign(v), Unit::Number),
        });
    }
    let cat = if op == Op::Abs { a.cat } else { C::Number };
    Some(Expr::op(op, args, cat, MathType::default()))
}

/// Build `op` over `args` again, simplifying as at parse time.
fn rebuild(op: Op, mut args: Vec<Expr>) -> Option<Expr> {
    match op {
        Op::Add | Op::Sub | Op::Mul | Op::Div => {
            if args.len() != 2 {
                return None;
            }
            let r = args.pop()?;
            let l = args.pop()?;
            arith(l, r, op)
        }
        Op::Invert => create_invert(args.pop()?),
        Op::Min | Op::Max | Op::Clamp => comparison(args, op),
        Op::RoundNearest | Op::RoundUp | Op::RoundDown | Op::RoundToZero | Op::Mod | Op::Rem => stepped_fn(args, op),
        Op::Sin | Op::Cos | Op::Tan | Op::Asin | Op::Acos | Op::Atan | Op::Atan2 => trigonometric(args, op),
        Op::Pow | Op::Sqrt | Op::Hypot | Op::Log | Op::Exp => exponential(args, op),
        Op::Abs | Op::Sign => sign_related(args, op),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Sums and products (`MaybeSimplifySumOrProductNode`, `CollectSumOrProductInOrder`)
// ---------------------------------------------------------------------------------------------------------------

/// `TraverseNumericChildrenFromNode`: the terms of a sum (or the factors of a product) in order, each with the
/// operator it is applied with, a subtraction distributed over a parenthesised sum.
fn terms(root: &Expr, op: Op) -> Vec<(Op, Expr)> {
    fn traverse(e: &Expr, op: Op) -> bool {
        e.op_kind().is_some_and(|k| if op == Op::Mul { k.is_mul_div() } else { k.is_add_sub() }) && !e.nested_intermediate()
    }
    fn push<'a>(stack: &mut Vec<(&'a Expr, Op, bool)>, e: &'a Expr, op: Op, nesting: bool) {
        let own = e.op_kind().unwrap_or(op);
        let nesting = nesting || e.nested;
        // `a - (b + c)` is `a - b - c`, `a - (b - c)` is `a - b + c`.
        let back = match (nesting, op, own) {
            (true, Op::Sub, Op::Add) => Op::Sub,
            (true, Op::Sub, Op::Sub) => Op::Add,
            _ => own,
        };
        let args = e.args();
        stack.push((&args[args.len() - 1], back, nesting));
        stack.push((&args[0], op, nesting));
    }
    let mut out = Vec::new();
    if !traverse(root, op) {
        out.push((op, root.clone()));
        return out;
    }
    let mut stack: Vec<(&Expr, Op, bool)> = Vec::new();
    push(&mut stack, root, op, false);
    while let Some((e, op, nesting)) = stack.pop() {
        if !traverse(e, op) {
            out.push((op, e.clone()));
            continue;
        }
        push(&mut stack, e, op, nesting);
    }
    out
}

/// `MaybeNegateFirstNode`.
fn negate_first(op: Op, e: Expr) -> Expr {
    match e.numeric() {
        Some((v, u)) if op == Op::Sub => Expr::literal(-v, u),
        _ => e,
    }
}

/// `MaybeSimplifySumOrProductNode`: like terms combined across the whole sum or product. Chromium 152's: a product's
/// plain numbers are collected into one factor, multiplied into the first literal of another unit, else put back
/// as the product's last factor (`calc(2 * sibling-index())`); a product of numbers alone is that number.
fn simplify_sum_or_product(root: Expr) -> Option<Expr> {
    let is_mul = root.op_kind().is_some_and(Op::is_mul_div);
    let all = terms(&root, if is_mul { Op::Mul } else { Op::Add });
    let mut combined: Vec<(Unit, f64)> = Vec::new();
    for (op, e) in &all {
        if let Some((v, u)) = e.numeric() {
            let v = if *op == Op::Sub { -v } else { v };
            match combined.iter_mut().find(|(cu, _)| *cu == u) {
                Some((_, acc)) => {
                    if *op == Op::Mul {
                        *acc *= v;
                    } else {
                        *acc += v;
                    }
                }
                None => combined.push((u, v)),
            }
        }
    }
    // the multiplicative constants, distributed onto the first literal of another unit
    let mut factor = if is_mul { combined.iter().find(|(u, _)| *u == Unit::Number).map_or(1.0, |&(_, v)| v) } else { 1.0 };
    let mut used: Vec<Unit> = Vec::new();
    let mut out: Option<Expr> = None;
    for (op, e) in &all {
        if is_mul && e.numeric().is_some_and(|(_, u)| u == Unit::Number) {
            continue;
        }
        // `MaybeReplaceNodeWithCombined`
        let (op, e) = match e.numeric() {
            Some((_, u)) => match combined.iter().find(|(cu, _)| *cu == u) {
                Some(&(_, v)) if is_mul => {
                    let v = v * factor;
                    factor = 1.0;
                    (*op, Expr::literal(v, u))
                }
                Some(&(_, v)) => (if v < 0.0 { Op::Sub } else { Op::Add }, Expr::literal(v.abs(), u)),
                None => (*op, e.clone()),
            },
            None => (*op, e.clone()),
        };
        if let Some((_, u)) = e.numeric() {
            if used.contains(&u) {
                continue;
            }
            used.push(u);
        }
        out = Some(match out {
            None => negate_first(op, e),
            Some(acc) => arith(acc, e, op)?,
        });
    }
    if is_mul {
        out = Some(match out {
            None => Expr::literal(factor, Unit::Number),
            Some(acc) if factor != 1.0 => arith(acc, Expr::literal(factor, Unit::Number), Op::Mul)?,
            Some(acc) => acc,
        });
    }
    let mut out = out.unwrap_or(root);
    out.nested = true;
    Some(out)
}

/// `MaybeSimplifySumOrProductNode` when `e` is a sum or product it may be applied to.
fn maybe_simplify(e: Expr) -> Option<Expr> {
    let applies = e.op_kind().is_some_and(|k| k.is_add_sub() || k.is_mul_div()) && !e.nested_intermediate();
    if applies {
        simplify_sum_or_product(e)
    } else {
        Some(e)
    }
}

/// `CollectSumOrProductInOrder`: a number, a percentage, the dimensions by unit name, then everything else.
fn in_order(root: &Expr) -> Vec<(Op, Expr)> {
    let is_mul = root.op_kind().is_some_and(Op::is_mul_div);
    let all = terms(root, if is_mul { Op::Mul } else { Op::Add });
    let mut groups: Vec<(Unit, Vec<(Op, Expr)>)> = Vec::new();
    let mut complex = Vec::new();
    for (op, e) in all {
        match e.numeric() {
            Some((_, u)) => match groups.iter_mut().find(|(g, _)| *g == u) {
                Some((_, list)) => list.push((op, e)),
                None => groups.push((u, vec![(op, e)])),
            },
            None => complex.push((op, e)),
        }
    }
    let mut ret = Vec::new();
    for first in [Unit::Number, Unit::Percent] {
        if let Some(i) = groups.iter().position(|(u, _)| *u == first) {
            ret.extend(groups.remove(i).1);
        }
    }
    groups.sort_by(|a, b| a.0.text().to_ascii_lowercase().cmp(&b.0.text().to_ascii_lowercase()));
    for (_, list) in groups {
        ret.extend(list);
    }
    ret.extend(complex);
    ret
}

// ---------------------------------------------------------------------------------------------------------------
// Serialization
// ---------------------------------------------------------------------------------------------------------------

/// `CSSNumericLiteralValue::CustomCSSText`.
pub fn literal_text(value: f64, unit: Unit) -> String {
    if unit == Unit::Integer {
        let i = if value.is_nan() { 0.0 } else { value.clamp(i32::MIN as f64, i32::MAX as f64) };
        return format!("{}", i as i32);
    }
    let text = unit.text();
    if !value.is_finite() {
        let base = if value.is_nan() {
            "NaN"
        } else if value > 0.0 {
            "infinity"
        } else {
            "-infinity"
        };
        return if text.is_empty() { base.to_string() } else { format!("{base} * 1{text}") };
    }
    format!("{}{text}", crate::style::serialize::format_number(value))
}

impl Expr {
    /// `CustomCSSText` of this node.
    fn node_text(&self) -> String {
        match &self.node {
            Node::Lit(v, u) => literal_text(*v, *u),
            Node::NoneKw => "none".to_string(),
            Node::Op(op, args, ..) => match op {
                Op::Add | Op::Sub | Op::Mul | Op::Div => {
                    let mut list = in_order(self);
                    if list.len() == 1 {
                        list = vec![(Op::Mul, args[0].clone()), (*op, args[args.len() - 1].clone())];
                    }
                    let mut out = String::from("(");
                    let (op0, e0) = list[0].clone();
                    out.push_str(&negate_first(op0, e0).node_text());
                    for (op, e) in list.into_iter().skip(1) {
                        // Chromium 152: in a product, an inverted operand `(1 / x)` is written `/ x` rather
                        // than `* (1 / x)` (#serialize-a-calculation-tree)
                        if op == Op::Mul && e.op_kind() == Some(Op::Invert) {
                            out.push_str(" / ");
                            out.push_str(&e.args()[0].node_text());
                            continue;
                        }
                        let (op, e) = match e.numeric() {
                            Some((v, u)) if op.is_add_sub() && v < 0.0 => {
                                (if op == Op::Add { Op::Sub } else { Op::Add }, Expr::literal(-v, u))
                            }
                            _ => (op, e),
                        };
                        out.push(' ');
                        out.push_str(op.text());
                        out.push(' ');
                        out.push_str(&e.node_text());
                    }
                    out.push(')');
                    out
                }
                Op::Invert => format!("(1 / {})", args[0].node_text()),
                op if op.is_round() => {
                    let mut out = String::from("round(");
                    match op {
                        Op::RoundUp => out.push_str("up, "),
                        Op::RoundDown => out.push_str("down, "),
                        Op::RoundToZero => out.push_str("to-zero, "),
                        _ => {}
                    }
                    out.push_str(&args[0].top_level_text());
                    // the step is left out when it is the default: a number rounded by the literal 1
                    let default_step = args[0].cat == C::Number && matches!(args[1].node, Node::Lit(v, u) if v == 1.0 && u.kind() == UnitKind::Number);
                    if !default_step {
                        out.push_str(", ");
                        out.push_str(&args[1].top_level_text());
                    }
                    out.push(')');
                    out
                }
                op => {
                    let inner: Vec<String> = args.iter().map(Expr::top_level_text).collect();
                    format!("{}({})", op.text(), inner.join(", "))
                }
            },
        }
    }

    /// `SerializeTopLevelNode`: an argument without its own outer parentheses.
    fn top_level_text(&self) -> String {
        let t = self.node_text();
        if t.starts_with('(') && t.ends_with(')') {
            t[1..t.len() - 1].to_string()
        } else {
            t
        }
    }

    /// `CSSMathFunctionValue::CustomCSSText`: what `style.x` reads back for this math function.
    pub fn css_text(&self) -> String {
        let t = self.node_text();
        if self.op_kind().is_some_and(Op::stands_alone) {
            return t;
        }
        if t.starts_with('(') {
            format!("calc{t}")
        } else {
            format!("calc({t})")
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Parsing (`CSSMathExpressionNodeParser`)
// ---------------------------------------------------------------------------------------------------------------

/// The math functions this file parses, by name.
pub fn is_math_function(name: &str) -> bool {
    function_op(name).is_some()
}

/// The function `name` is, as the operation it builds (`calc` and `-webkit-calc` build `Add`'s stand-in).
fn function_op(name: &str) -> Option<Func> {
    let lower = name.to_ascii_lowercase();
    Some(match lower.as_str() {
        "calc" | "-webkit-calc" => Func::Calc,
        "min" => Func::Op(Op::Min),
        "max" => Func::Op(Op::Max),
        "clamp" => Func::Op(Op::Clamp),
        "sin" => Func::Op(Op::Sin),
        "cos" => Func::Op(Op::Cos),
        "tan" => Func::Op(Op::Tan),
        "asin" => Func::Op(Op::Asin),
        "acos" => Func::Op(Op::Acos),
        "atan" => Func::Op(Op::Atan),
        "atan2" => Func::Op(Op::Atan2),
        "round" => Func::Op(Op::RoundNearest),
        "mod" => Func::Op(Op::Mod),
        "rem" => Func::Op(Op::Rem),
        "pow" => Func::Op(Op::Pow),
        "sqrt" => Func::Op(Op::Sqrt),
        "hypot" => Func::Op(Op::Hypot),
        "log" => Func::Op(Op::Log),
        "exp" => Func::Op(Op::Exp),
        "abs" => Func::Op(Op::Abs),
        "sign" => Func::Op(Op::Sign),
        _ => return None,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Func {
    Calc,
    Op(Op),
}

/// Parser state that varies down the tree (`CSSMathExpressionNodeParser::State`).
#[derive(Debug, Clone, Copy)]
struct State {
    depth: u32,
    clamp_none: bool,
    percent: bool,
}

type PResult<'i, T> = Result<T, ParseError<'i, ()>>;

/// Parse ONE math function at the parser's position -- the function token and its block -- and build it. `None` is
/// a value Blink rejects: an unknown function, a syntax error, a type mismatch. A percentage is allowed.
pub fn parse_function(p: &mut Parser) -> Option<Expr> {
    parse_function_with(p, true)
}

/// [`parse_function`], percentages allowed or not (`CSSMathExpressionNode::Flag::AllowPercent`).
pub fn parse_function_with(p: &mut Parser, allow_percent: bool) -> Option<Expr> {
    let name = match p.next_including_whitespace() {
        Ok(Token::Function(name)) => name.clone(),
        _ => return None,
    };
    let f = function_op(&name)?;
    let state = State { depth: 0, clamp_none: false, percent: allow_percent };
    p.parse_nested_block(|p| -> PResult<'_, Expr> {
        skip_ws(p);
        math_function(p, f, state).ok_or_else(|| p.new_custom_error(()))
    })
    .ok()
}

/// One math function written as `text`, and nothing else around it (whitespace aside).
pub fn parse(text: &str) -> Option<Expr> {
    let mut input = ParserInput::new(text.trim());
    let mut p = Parser::new(&mut input);
    let e = parse_function(&mut p)?;
    p.expect_exhausted().ok()?;
    Some(e)
}

/// Consume whitespace; whether there was any.
fn skip_ws(p: &mut Parser) -> bool {
    let mut any = false;
    loop {
        let st = p.state();
        match p.next_including_whitespace() {
            Ok(Token::WhiteSpace(_)) => any = true,
            _ => {
                p.reset(&st);
                return any;
            }
        }
    }
}

/// The next token, not consumed.
fn peek<'i>(p: &mut Parser<'i, '_>) -> Option<Token<'i>> {
    let st = p.state();
    let t = p.next_including_whitespace().ok().cloned();
    p.reset(&st);
    t
}

fn at_end(p: &mut Parser) -> bool {
    peek(p).is_none()
}

/// `ConsumeCommaIncludingWhitespace`.
fn comma(p: &mut Parser) -> bool {
    if matches!(peek(p), Some(Token::Comma)) {
        let _ = p.next_including_whitespace();
        skip_ws(p);
        true
    } else {
        false
    }
}

/// `ParseMathFunction`, inside the function's block.
fn math_function(p: &mut Parser, f: Func, mut state: State) -> Option<Expr> {
    state.clamp_none = false;
    let (min, max) = match f {
        Func::Calc => (1, 1),
        Func::Op(op) => match op {
            Op::Min | Op::Max => (1, usize::MAX),
            Op::Clamp => {
                state.clamp_none = true;
                (3, 3)
            }
            Op::Sin | Op::Cos | Op::Tan | Op::Asin | Op::Acos | Op::Atan | Op::Exp | Op::Sqrt => (1, 1),
            Op::Pow | Op::Atan2 | Op::Mod | Op::Rem => (2, 2),
            Op::Hypot => (1, MAX_DEPTH as usize),
            Op::Log | Op::RoundNearest => (1, 2),
            Op::Abs | Op::Sign => (1, 1),
            _ => (1, 1),
        },
    };
    // round()'s optional <rounding-strategy>, then a comma.
    let mut op = match f {
        Func::Op(op) => op,
        Func::Calc => Op::Add,
    };
    if op == Op::RoundNearest {
        let strategy = match peek(p) {
            Some(Token::Ident(name)) => match name.to_ascii_lowercase().as_str() {
                "nearest" => Some(Op::RoundNearest),
                "up" => Some(Op::RoundUp),
                "down" => Some(Op::RoundDown),
                "to-zero" => Some(Op::RoundToZero),
                _ => None,
            },
            _ => None,
        };
        if let Some(s) = strategy {
            let _ = p.next_including_whitespace();
            skip_ws(p);
            if !comma(p) {
                return None;
            }
            op = s;
        }
    }
    let mut args: Vec<Expr> = Vec::new();
    while !at_end(p) && args.len() < max {
        if !args.is_empty() && !comma(p) {
            return None;
        }
        skip_ws(p);
        args.push(value_expression(p, state)?);
    }
    if !at_end(p) || args.len() < min {
        return None;
    }
    match f {
        Func::Calc => args.pop(),
        Func::Op(_) => match op {
            Op::Min | Op::Max | Op::Clamp => comparison(args, op),
            Op::Sin | Op::Cos | Op::Tan | Op::Asin | Op::Acos | Op::Atan | Op::Atan2 => trigonometric(args, op),
            Op::Pow | Op::Sqrt | Op::Hypot | Op::Log | Op::Exp => exponential(args, op),
            Op::RoundNearest | Op::RoundUp | Op::RoundDown | Op::RoundToZero => {
                // `CanonicalizeRoundArguments`: B may be left out only when A is a number, and is 1 then.
                if args.len() == 1 {
                    if args[0].cat != C::Number {
                        return None;
                    }
                    args.push(Expr::literal(1.0, Unit::Integer));
                }
                stepped_fn(args, op)
            }
            Op::Mod | Op::Rem => stepped_fn(args, op),
            Op::Abs | Op::Sign => sign_related(args, op),
            _ => None,
        },
    }
}

/// `ParseValueExpression`.
fn value_expression(p: &mut Parser, mut state: State) -> Option<Expr> {
    state.depth += 1;
    if state.depth > MAX_DEPTH {
        return None;
    }
    additive(p, state)
}

/// `ParseAdditiveValueExpression`: `+` and `-` need whitespace on both sides (`calc(1px+ 2px)` and
/// `calc(1px +2px)` are invalid).
fn additive(p: &mut Parser, state: State) -> Option<Expr> {
    if at_end(p) {
        return None;
    }
    let (mut result, mut ws_after) = multiplicative(p, state)?;
    let mut depth = state.depth + 1;
    while !at_end(p) {
        let op = match peek(p) {
            Some(Token::Delim('+')) => Op::Add,
            Some(Token::Delim('-')) => Op::Sub,
            _ => break,
        };
        if !ws_after {
            return None;
        }
        let _ = p.next_including_whitespace();
        if !matches!(peek(p), Some(Token::WhiteSpace(_))) {
            return None;
        }
        skip_ws(p);
        let (rhs, ws) = multiplicative(p, state)?;
        ws_after = ws;
        result = arith(result, rhs, op)?;
        if result.is_op() {
            depth += 1;
            if depth > MAX_DEPTH {
                return None;
            }
        } else {
            depth = state.depth + 1;
        }
    }
    if result.op_kind().is_some_and(Op::is_add_sub) {
        result = maybe_simplify(result)?;
    }
    Some(result)
}

/// `ParseValueMultiplicativeExpression`.
fn multiplicative(p: &mut Parser, state: State) -> Option<(Expr, bool)> {
    if at_end(p) {
        return None;
    }
    let (mut result, mut ws_after) = term(p, state)?;
    let mut depth = state.depth + 1;
    while !at_end(p) {
        let op = match peek(p) {
            Some(Token::Delim('*')) => Op::Mul,
            Some(Token::Delim('/')) => Op::Div,
            _ => break,
        };
        let _ = p.next_including_whitespace();
        skip_ws(p);
        let (rhs, ws) = term(p, state)?;
        ws_after = ws;
        result = arith(result, rhs, op)?;
        if result.is_op() {
            depth += 1;
            if depth > MAX_DEPTH {
                return None;
            }
        } else {
            depth = state.depth + 1;
        }
    }
    if result.op_kind().is_some_and(Op::is_mul_div) {
        result = maybe_simplify(result)?;
    }
    Some((result, ws_after))
}

/// `ParseValueTerm`: a parenthesised sum, a nested `calc()`, another math function, or a value.
fn term(p: &mut Parser, state: State) -> Option<(Expr, bool)> {
    let next = peek(p)?;
    let nested_sum = match &next {
        Token::ParenthesisBlock => true,
        Token::Function(name) => name.eq_ignore_ascii_case("calc"),
        _ => false,
    };
    if nested_sum {
        let _ = p.next_including_whitespace();
        let result = p
            .parse_nested_block(|p| -> PResult<'_, Expr> {
                skip_ws(p);
                let r = value_expression(p, state).ok_or_else(|| p.new_custom_error(()))?;
                // `none` bounds a clamp() only as a whole argument, never in parentheses.
                if r.is_none_kw() {
                    return Err(p.new_custom_error(()));
                }
                Ok(r)
            })
            .ok()?;
        let mut result = result;
        result.nested = true;
        let ws = skip_ws(p);
        return Some((result, ws));
    }
    if let Token::Function(name) = &next {
        let f = function_op(name)?;
        let _ = p.next_including_whitespace();
        let result = p
            .parse_nested_block(|p| -> PResult<'_, Expr> {
                skip_ws(p);
                math_function(p, f, state).ok_or_else(|| p.new_custom_error(()))
            })
            .ok()?;
        let ws = skip_ws(p);
        return Some((result, ws));
    }
    if matches!(next, Token::SquareBracketBlock | Token::CurlyBracketBlock) {
        return None;
    }
    value(p, state)
}

/// `ParseValue`: a number, a percentage, a dimension, or one of the constants.
fn value(p: &mut Parser, state: State) -> Option<(Expr, bool)> {
    let start = p.position();
    let token = p.next_including_whitespace().ok()?.clone();
    let source = p.slice_from(start).to_string();
    let ws = skip_ws(p);
    let e = match token {
        Token::Ident(name) => match name.to_ascii_lowercase().as_str() {
            "infinity" => Expr::literal(f64::INFINITY, Unit::Number),
            "-infinity" => Expr::literal(f64::NEG_INFINITY, Unit::Number),
            "nan" => Expr::literal(f64::NAN, Unit::Number),
            "pi" => Expr::literal(std::f64::consts::PI, Unit::Number),
            "e" => Expr::literal(std::f64::consts::E, Unit::Number),
            "none" if state.clamp_none => Expr { node: Node::NoneKw, cat: C::Ident, nested: false },
            _ => return None,
        },
        Token::Number { .. } => {
            let (v, integer) = number_of(&source)?;
            Expr::literal(v, if integer { Unit::Integer } else { Unit::Number })
        }
        Token::Percentage { .. } if state.percent => Expr::literal(number_of(&source)?.0, Unit::Percent),
        Token::Dimension { ref unit, .. } => Expr::literal(number_of(&source)?.0, Unit::dimension(unit)?),
        _ => return None,
    };
    Some((e, ws))
}

/// The value of the numeric token whose source text is `source`, at full double precision -- cssparser's own value
/// is an f32, and `cos(-30780.497322536891)` is not `cos(-30780.498046875)` -- and whether it is written as an
/// integer (no fraction, no exponent). A comment the tokenizer skipped may come first.
pub fn number_of(source: &str) -> Option<(f64, bool)> {
    let mut s = source;
    while let Some(rest) = s.strip_prefix("/*") {
        s = &rest[rest.find("*/")? + 2..];
    }
    let b = s.as_bytes();
    let mut i = 0;
    if matches!(b.first(), Some(b'+' | b'-')) {
        i += 1;
    }
    let digits = |i: &mut usize| {
        let from = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        *i > from
    };
    let mut integer = true;
    let whole = digits(&mut i);
    if i + 1 < b.len() && b[i] == b'.' && b[i + 1].is_ascii_digit() {
        i += 1;
        digits(&mut i);
        integer = false;
    } else if !whole {
        return None;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        if j < b.len() && b[j].is_ascii_digit() {
            i = j;
            digits(&mut i);
            integer = false;
        }
    }
    let v: f64 = s[..i].parse().ok()?;
    Some((v, integer))
}

// ---------------------------------------------------------------------------------------------------------------
// Callers' helpers
// ---------------------------------------------------------------------------------------------------------------

/// Does `text` hold a math function (`calc(`, `min(`, `sin(`, ...) anywhere?
pub fn mentions_math(text: &str) -> bool {
    if !text.contains('(') {
        return false;
    }
    let lower = text.to_ascii_lowercase();
    let b = lower.as_bytes();
    lower.match_indices('(').any(|(at, _)| {
        let mut s = at;
        while s > 0 && (b[s - 1].is_ascii_alphanumeric() || b[s - 1] == b'-') {
            s -= 1;
        }
        is_math_function(&lower[s..at])
    })
}

/// `text` with every math function in it -- at any depth, `rotate(calc(1turn / 4))` too -- written the way Blink
/// serializes the parsed value (`rotate(calc(90deg))`). A function that does not parse is left as written.
pub fn serialize_math_in(text: &str) -> String {
    if !mentions_math(text) {
        return text.to_string();
    }
    rewrite_math(text, |e| e.css_text())
}

/// `text` with every math function that simplified to ONE literal replaced by that literal at full precision, for
/// a computed-value step that reads plain values (`opacity: calc(1 / 3)` is `0.3333333333333333`,
/// `transform: rotate(atan2(1, 1))` is `rotate(45deg)`). `integer` rounds a number half towards positive infinity
/// (Blink's `ClampToPermittedRange` for an `<integer>` property); `non_negative` floors it at zero.
pub fn substitute_literals(text: &str, integer: bool, non_negative: bool) -> String {
    if !mentions_math(text) {
        return text.to_string();
    }
    let mut changed = false;
    let out = rewrite_math(text, |e| match e.as_literal() {
        Some((v, u)) if v.is_finite() => {
            changed = true;
            let mut v = if non_negative { v.max(0.0) } else { v };
            if integer && u.kind() == UnitKind::Number {
                v = (v + 0.5).floor();
            }
            // a time reads back as its computed value is written (`0.5s`, not `0.49999999999999994s`); a length or an
            // angle is laid out or transformed from, at full precision
            let n = if u.kind() == UnitKind::Time {
                crate::style::serialize::format_number(v)
            } else if v == 0.0 {
                "0".to_string()
            } else {
                format!("{v}")
            };
            format!("{n}{}", u.text())
        }
        _ => e.css_text(),
    });
    if changed {
        out
    } else {
        text.to_string()
    }
}

/// A colour function: a math function inside one may name its channels (`rgb(from red calc(r * 2) g b)`), which
/// is the colour parser's to resolve (`css_color`), so it is left alone here.
fn is_color_function(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "rgb" | "rgba" | "hsl" | "hsla" | "hwb" | "lab" | "lch" | "oklab" | "oklch" | "color" | "color-mix"
            | "contrast-color" | "light-dark" | "device-cmyk"
    )
}

/// Consume a block without looking inside.
fn skip_block(p: &mut Parser) {
    let _ = p.parse_nested_block(|p| -> PResult<'_, ()> {
        while p.next_including_whitespace_and_comments().is_ok() {}
        Ok(())
    });
}

/// Every math function in `text` replaced by `f` of its parsed expression.
fn rewrite_math(text: &str, mut f: impl FnMut(&Expr) -> String) -> String {
    let mut input = ParserInput::new(text);
    let mut p = Parser::new(&mut input);
    let mut out = String::with_capacity(text.len());
    let mut copied = 0usize;
    walk(&mut p, text, &mut out, &mut copied, &mut f);
    out.push_str(&text[copied..]);
    out
}

fn walk(p: &mut Parser, text: &str, out: &mut String, copied: &mut usize, f: &mut dyn FnMut(&Expr) -> String) {
    loop {
        let st = p.state();
        let start = p.position().byte_index();
        let token = match p.next_including_whitespace_and_comments() {
            Ok(t) => t.clone(),
            Err(_) => return,
        };
        match token {
            Token::Function(ref name) if is_math_function(name) => {
                p.reset(&st);
                let parsed = parse_function(p);
                let end = p.position().byte_index();
                if let Some(e) = parsed {
                    if start >= *copied && end <= text.len() {
                        out.push_str(&text[*copied..start]);
                        out.push_str(&f(&e));
                        *copied = end;
                    }
                }
            }
            Token::Function(ref name) if is_color_function(name) => skip_block(p),
            Token::Function(_) | Token::ParenthesisBlock | Token::SquareBracketBlock | Token::CurlyBracketBlock => {
                let _ = p.parse_nested_block(|p| -> PResult<'_, ()> {
                    walk(p, text, out, copied, f);
                    Ok(())
                });
            }
            _ => {}
        }
    }
}

/// Does `text` call a function that is not a math function this file ports -- `var()`, `env()`, `attr()`,
/// `anchor()`, `sibling-index()`, `progress()`, ...? Inside a math function such a call is either substituted before
/// the expression is parsed or one of Blink's functions this port leaves out, so the expression is not judged here.
pub fn has_foreign_function(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let b = lower.as_bytes();
    lower.match_indices('(').any(|(at, _)| {
        let mut s = at;
        while s > 0 && (b[s - 1].is_ascii_alphanumeric() || b[s - 1] == b'-' || b[s - 1] == b'_') {
            s -= 1;
        }
        s < at && !is_math_function(&lower[s..at])
    })
}

/// Is every math function in `text` one Blink accepts (it parses and type-checks)? A value with a math function
/// that does not -- `calc(1s + 1px)`, `round(10.4px)`, `calc(1px+2px)` -- is an invalid declaration. A math function
/// that holds a function this file does not port ([`has_foreign_function`]) is not judged.
pub fn math_valid(text: &str) -> bool {
    if !mentions_math(text) {
        return true;
    }
    let mut ok = true;
    let mut input = ParserInput::new(text);
    let mut p = Parser::new(&mut input);
    check(&mut p, &mut ok);
    ok
}

fn check(p: &mut Parser, ok: &mut bool) {
    loop {
        let st = p.state();
        let token = match p.next_including_whitespace_and_comments() {
            Ok(t) => t.clone(),
            Err(_) => return,
        };
        match token {
            Token::Function(ref name) if is_math_function(name) => {
                p.reset(&st);
                let start = p.position();
                if parse_function(p).is_none() && !has_foreign_function(p.slice_from(start)) {
                    *ok = false;
                }
            }
            Token::Function(ref name) if is_color_function(name) => skip_block(p),
            Token::Function(_) | Token::ParenthesisBlock | Token::SquareBracketBlock | Token::CurlyBracketBlock => {
                let _ = p.parse_nested_block(|p| -> PResult<'_, ()> {
                    check(p, ok);
                    Ok(())
                });
            }
            _ => {}
        }
    }
}

/// `text` is exactly one math function (whitespace aside): its expression. `None` also for a value that is not one.
pub fn single(text: &str) -> Option<Expr> {
    let t = text.trim();
    let open = t.find('(')?;
    if !is_math_function(&t[..open]) || !t.ends_with(')') {
        return None;
    }
    parse(t)
}

/// Is `text` one math function of one of the categories in `cats`? (`ConsumeLengthOrPercent` and friends: the
/// function parses, and its category is one the grammar takes.)
pub fn single_of(text: &str, cats: &[Cat]) -> bool {
    single(text).is_some_and(|e| cats.contains(&e.cat))
}

/// The categories a `<length-percentage>` takes from a math function.
pub const LENGTH_PERCENTAGE: &[Cat] = &[C::Length, C::Percent, C::LengthFunction];

// ---------------------------------------------------------------------------------------------------------------
// The computed side
// ---------------------------------------------------------------------------------------------------------------

impl Expr {
    /// The expression with every relative length made pixels by `px(value, unit)`, simplified again as Blink
    /// simplifies a calculation whose lengths are known (`CSSMathExpressionNode::ToCalculationExpression`).
    pub fn absolutized(&self, px: &dyn std::ops::Fn(f64, Unit) -> f64) -> Option<Expr> {
        match &self.node {
            Node::Lit(v, u) if u.is_relative() => Some(Expr::literal(px(*v, *u), Unit::Px)),
            Node::Lit(..) | Node::NoneKw => Some(self.clone()),
            Node::Op(op, args, ..) => {
                let args: Option<Vec<Expr>> = args.iter().map(|a| a.absolutized(px)).collect();
                let mut e = rebuild(*op, args?)?;
                e.nested = self.nested;
                maybe_simplify(e)
            }
        }
    }

    /// The value, in the category's canonical unit, a percentage being that share of `basis`. Relative lengths must
    /// have been resolved ([`Expr::absolutized`]).
    pub fn eval(&self, basis: f64) -> f64 {
        match &self.node {
            Node::Lit(v, u) => match u.kind() {
                UnitKind::Percent => v / 100.0 * basis,
                UnitKind::Number => *v,
                _ => v * u.factor(),
            },
            Node::NoneKw => f64::NAN,
            Node::Op(op, args, ..) => {
                let values: Vec<f64> = args
                    .iter()
                    .map(|a| {
                        let v = a.eval(basis);
                        if matches!(op, Op::Sin | Op::Cos | Op::Tan) && a.cat == C::Number {
                            rad2deg(v)
                        } else {
                            v
                        }
                    })
                    .collect();
                evaluate(&values, *op)
            }
        }
    }

    /// `percent% + px` when the expression is that linear form (any nesting of sums, differences and products by a
    /// number over percentages and pixels), which is what layout's fast path takes.
    pub fn linear(&self) -> Option<(f64, f64)> {
        match &self.node {
            Node::Lit(v, Unit::Percent) => Some((*v, 0.0)),
            Node::Lit(v, u) if u.kind() == UnitKind::Length && !u.is_relative() => Some((0.0, v * u.factor())),
            Node::Op(Op::Add, args, ..) | Node::Op(Op::Sub, args, ..) => {
                let (a, b) = (args[0].linear()?, args[1].linear()?);
                let k = if self.op_kind() == Some(Op::Sub) { -1.0 } else { 1.0 };
                Some((a.0 + k * b.0, a.1 + k * b.1))
            }
            Node::Op(Op::Mul, args, ..) => {
                let (n, other) = if args[0].cat == C::Number { (&args[0], &args[1]) } else { (&args[1], &args[0]) };
                let k = n.as_literal().filter(|(_, u)| *u == Unit::Number)?.0;
                let (pct, px) = other.linear()?;
                Some((pct * k, px * k))
            }
            _ => None,
        }
    }

    /// `min(...)`/`max(...)`/`clamp(...)` as its operator and operands, for layout's comparison nodes.
    pub fn comparison(&self) -> Option<(Op, &[Expr])> {
        match &self.node {
            Node::Op(op @ (Op::Min | Op::Max | Op::Clamp), args, ..) => Some((*op, args.as_slice())),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(v: &str) -> String {
        parse(v).map(|e| e.css_text()).unwrap_or_default()
    }

    /// Chrome 152's own `style.width` answers (`fixtures/css-answers/calc-chrome-152.json`).
    #[test]
    fn simplified_and_serialized_as_chrome_152_reads_them_back() {
        for (written, chrome) in [
            ("calc( 1px * ( sin( 66911823500 * ( 36781 / -0.55 * cos( -30780.497322536891 ) ) ) )", "calc(0.207912px)"),
            ("calc(1px * sin(0.5))", "calc(0.479426px)"),
            ("calc(sin(30deg) * 10px)", "calc(5px)"),
            ("calc(1px * cos(0))", "calc(1px)"),
            ("calc(1px * pi)", "calc(3.14159px)"),
            ("calc(1px * e)", "calc(2.71828px)"),
            ("calc(1px * sqrt(2))", "calc(1.41421px)"),
            ("calc(10px * tan(45deg))", "calc(10px)"),
            ("calc(1px * pow(2, 3))", "calc(8px)"),
            ("hypot(3px, 4px)", "calc(5px)"),
            ("calc(1px * log(8, 2))", "calc(3px)"),
            ("round(10.4px, 1px)", "calc(10px)"),
            ("round(up, 10.4px, 1px)", "calc(11px)"),
            ("round(to-zero, -10.6px, 1px)", "calc(-10px)"),
            ("round(nearest, 15px, 10px)", "calc(20px)"),
            ("mod(-10px, 3px)", "calc(2px)"),
            ("rem(-10px, 3px)", "calc(-1px)"),
            ("clamp(none, 5px, 3px)", "calc(3px)"),
            ("clamp(4px, 2px, none)", "calc(4px)"),
            ("min(1px)", "calc(1px)"),
            ("calc(50% - 1px * sin(1))", "calc(50% - 0.841471px)"),
            ("calc(10px + 50%)", "calc(50% + 10px)"),
            ("calc(1em + 1px + 1%)", "calc(1% + 1em + 1px)"),
            ("calc(2 * (10px + 5px))", "calc(30px)"),
            ("calc(1in + 1cm)", "calc(133.795px)"),
            ("calc(1pt + 1pc + 1mm + 1q)", "calc(22.0577px)"),
            ("calc(10px + 1em - 2px)", "calc(1em + 8px)"),
            ("calc(min(10%, 20px) + 5px)", "calc(5px + min(10%, 20px))"),
            ("calc(1px / 3)", "calc(0.333333px)"),
            ("calc(10px * -1 + 50%)", "calc(50% - 10px)"),
            ("calc(100% - 0px)", "calc(100% + 0px)"),
            ("calc(0% + 10px)", "calc(0% + 10px)"),
            ("calc(infinity * 1px)", "calc(infinity * 1px)"),
            ("calc(NaN * 1px)", "calc(NaN * 1px)"),
            ("calc(1px * 1px / 1px)", "calc((1px * 1px) / 1px)"),
            ("calc(1px / 1px * 10px)", "calc((1px / 1px) * 10px)"),
            ("calc(10px / 0)", "calc(infinity * 1px)"),
            ("-webkit-calc(1px + 2px)", "calc(3px)"),
            ("CALC(1PX + 2PX)", "calc(3px)"),
            ("calc(1px * sin(100grad))", "calc(1px)"),
            ("calc(1px * acos(0.5) / 1deg)", "calc((1px * 60deg) / 1deg)"),
            ("calc(1rem + 1ex)", "calc(1ex + 1rem)"),
            ("calc(10px * sign(50% - 10px))", "calc(10px * sign(50% - 10px))"),
            ("round(50%, 7px)", "round(50%, 7px)"),
            ("abs(50% - 600px)", "abs(50% - 600px)"),
            ("calc(1px * pow(10, 30))", "calc(1e+30px)"),
            ("calc(1px * 0.0000123)", "calc(1.23e-05px)"),
            ("calc(1px * round(-2.5))", "calc(-2px)"),
            ("calc(1px * sqrt(4px * 1px / 1px / 1px))", "calc(1px * sqrt(((4px * 1px) / 1px) / 1px))"),
            ("calc(1px * sqrt(-1))", "calc(NaN * 1px)"),
            ("calc(1s + 500ms)", "calc(1.5s)"),
            ("calc(1turn / 4)", "calc(90deg)"),
            ("calc(1rad)", "calc(57.2958deg)"),
            ("atan2(1, 1)", "calc(45deg)"),
            ("round(0.66, 0.1)", "calc(0.7)"),
            ("clamp(0, 1.5, 1)", "calc(1)"),
        ] {
            assert_eq!(text(written), chrome, "{written}");
        }
    }

    /// What Chrome 150 drops: a type error, a function that wants another type, `+` without whitespace.
    #[test]
    fn what_chrome_150_rejects_is_rejected_or_not_a_length() {
        for v in ["round(10.4px)", "calc(1px+2px)", "calc(1px +2px)", "calc(1s + 1px)", "calc(0.5 + 10%)", "calc(1 + 2px)"] {
            assert!(parse(v).is_none(), "{v}");
        }
        for v in ["calc(1px * atan2(1, 1))", "calc(1px * asin(1))", "calc(1px * 1px)"] {
            assert!(!single_of(v, LENGTH_PERCENTAGE), "{v}");
        }
    }

    #[test]
    fn a_layout_value_evaluates_against_its_basis() {
        let e = parse("calc(50% - 1px * sin(1))").unwrap();
        assert!((e.eval(1000.0) - 499.158529).abs() < 1e-5);
        assert_eq!(parse("round(50%, 7px)").unwrap().eval(1000.0), 497.0);
        assert_eq!(parse("mod(50%, 7px)").unwrap().eval(1000.0), 3.0);
        assert_eq!(parse("calc(10px * sign(50% - 10px))").unwrap().eval(1000.0), 10.0);
        let em = parse("calc(1em + 1px + 1%)").unwrap().absolutized(&|v, _| v * 16.0).unwrap();
        assert_eq!(em.linear(), Some((1.0, 17.0)));
    }

    #[test]
    fn literals_and_substitution() {
        assert_eq!(substitute_literals("rotate(atan2(1, 1))", false, false), "rotate(45deg)");
        assert_eq!(substitute_literals("calc(1.5)", true, false), "2");
        assert_eq!(substitute_literals("calc(-1.5)", true, false), "-1");
        assert_eq!(substitute_literals("calc(-1s)", false, true), "0s");
        assert_eq!(serialize_math_in("rotate(calc(1turn / 4))"), "rotate(calc(90deg))");
        assert_eq!(number_of("-30780.497322536891"), Some((-30780.497322536891, false)));
        assert_eq!(number_of("1e3px"), Some((1000.0, false)));
        assert_eq!(number_of("66911823500"), Some((66911823500.0, true)));
    }
}
