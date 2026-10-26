//! CSS MATH FUNCTIONS -- `calc()`, `min()`, `max()`, `clamp()` and the rest -- for lengths and percentages.
//!
//! Neither the computed-value step nor layout understood one: `lengthy` handed `calc(100% - 20px)` back as
//! written and `layout::style` read anything it could not parse as `auto` (or zero), so every math function in
//! a box's geometry vanished -- measured on Chrome 150 (`c8/repro/vhcalc`): `height: max(100vh, 700px)` 991px
//! there and 0 here, `width: calc(100% - 20px)` 980px and 1000px, even `calc(1px + 2px)`. tuta.com's hero,
//! `calc(1200px + 120vh)`, came out 467px tall instead of 2322px, so everything under it was laid out two
//! thousand pixels too high and viewport-triggered code ran where Chrome's did not (L8).
//!
//! A math function is parsed into a tree whose leaves are pixels and percentages (every other unit is resolved
//! on the spot with [`ResolveContext`]) and whose sums and scalar products fold into [`Node::Linear`]:
//! `a% + b px`. What is left is a pure length, which computes to pixels, or a tree over percentages, which only
//! layout can finish: it is serialized canonically for the computed value and evaluated against its basis by
//! layout's calc hook ([`Node::eval`], `layout::style`).
//!
//! THE PARSING IS `style::calc`'s: Blink's own math-expression parser, its types and its
//! simplification, every math function Chrome 150 takes (`round()`, `mod()`, `sin()`, `pow()`, ... as well as the
//! four this file once parsed itself). The expression is parsed, its relative lengths made pixels with the
//! [`ResolveContext`], simplified again, and then held here as the linear `a% + b px` layout's fast path takes, a
//! comparison of such, or -- `round(50%, 7px)`, `calc(10px * sign(50% - 10px))` -- the calculation itself, which
//! [`Node::eval`] evaluates against the basis (Chrome 150 in a 1000px box: `497px` and `10px`).

use crate::style::calc::{self, Expr, Op, Unit};
use crate::value::ResolveContext;

/// A reduced math expression: `pct` percent of the basis plus `px` pixels, a comparison of such, or a calculation
/// layout evaluates as it stands.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Linear { pct: f32, px: f32 },
    Min(Vec<Node>),
    Max(Vec<Node>),
    Clamp(Box<Node>, Box<Node>, Box<Node>),
    /// Anything else: `round()`, `mod()`, `abs()`, a product with a `sign()` -- its lengths already pixels.
    Calc(Box<Expr>),
}

impl Node {
    fn linear(pct: f32, px: f32) -> Node {
        Node::Linear { pct, px }
    }

    /// Whether any leaf is a percentage (then only layout knows the value).
    pub fn has_percentage(&self) -> bool {
        match self {
            Node::Linear { pct, .. } => *pct != 0.0,
            Node::Min(v) | Node::Max(v) => v.iter().any(Node::has_percentage),
            Node::Clamp(a, b, c) => a.has_percentage() || b.has_percentage() || c.has_percentage(),
            Node::Calc(e) => e.has_percent(),
        }
    }

    /// The value against `basis` (the containing block's size on the property's axis).
    pub fn eval(&self, basis: f32) -> f32 {
        match self {
            Node::Linear { pct, px } => pct / 100.0 * basis + px,
            Node::Min(v) => v.iter().map(|n| n.eval(basis)).fold(f32::INFINITY, f32::min),
            Node::Max(v) => v.iter().map(|n| n.eval(basis)).fold(f32::NEG_INFINITY, f32::max),
            // CSS Values 4: clamp(MIN, VAL, MAX) is max(MIN, min(VAL, MAX)).
            Node::Clamp(lo, v, hi) => lo.eval(basis).max(v.eval(basis).min(hi.eval(basis))),
            Node::Calc(e) => e.eval(basis as f64) as f32,
        }
    }

    /// The value when there is no percentage in it.
    pub fn px(&self) -> Option<f32> {
        (!self.has_percentage()).then(|| self.eval(0.0))
    }

    /// Chrome's serialization of the computed value: `calc(50% + 20px)`, `min(100%, 300px)`, `50%`.
    pub fn serialize(&self) -> String {
        match self {
            Node::Linear { pct, px } if *px == 0.0 => format!("{}%", num(*pct)),
            Node::Linear { pct, px } if *pct == 0.0 => format!("{}px", num(*px)),
            Node::Linear { pct, px } => {
                format!("calc({}% {} {}px)", num(*pct), if *px < 0.0 { "-" } else { "+" }, num(px.abs()))
            }
            Node::Min(v) => format!("min({})", v.iter().map(Node::inner).collect::<Vec<_>>().join(", ")),
            Node::Max(v) => format!("max({})", v.iter().map(Node::inner).collect::<Vec<_>>().join(", ")),
            Node::Clamp(a, b, c) => format!("clamp({}, {}, {})", a.inner(), b.inner(), c.inner()),
            Node::Calc(e) => e.css_text(),
        }
    }

    /// An argument inside a comparison: a sum is written without its own `calc()`.
    fn inner(&self) -> String {
        match self {
            Node::Linear { pct, px } if *pct != 0.0 && *px != 0.0 => {
                format!("{}% {} {}px", num(*pct), if *px < 0.0 { "-" } else { "+" }, num(px.abs()))
            }
            Node::Calc(e) => {
                let t = e.css_text();
                match t.strip_prefix("calc(").and_then(|s| s.strip_suffix(')')) {
                    Some(inner) => inner.to_string(),
                    None => t,
                }
            }
            n => n.serialize(),
        }
    }
}

fn num(v: f32) -> String {
    let r = (v * 1000.0).round() / 1000.0;
    if r == r.trunc() {
        format!("{}", r as i64)
    } else {
        let s = format!("{r:.3}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// Whether `text` is one math function and nothing else (after trimming) -- any `style::calc` parses.
pub fn is_math(text: &str) -> bool {
    let t = text.trim_start();
    t.find('(').is_some_and(|open| calc::is_math_function(&t[..open]))
}

/// One relative length in pixels, against `ctx`. The viewport variants are the viewport on a desktop browser, and
/// the inline and block viewport units its width and height in `horizontal-tb`; `None` for a font metric this
/// context does not carry (`lh`, `cap`, `ic`, the root's `ex`/`ch`).
fn relative_px(value: f64, unit: Unit, ctx: &ResolveContext) -> Option<f32> {
    use crate::value::{Length, LengthUnit};
    let unit = match unit {
        Unit::Em => LengthUnit::Em,
        Unit::Ex => LengthUnit::Ex,
        Unit::Ch => LengthUnit::Ch,
        Unit::Rem => LengthUnit::Rem,
        Unit::Vw | Unit::Vi | Unit::Svw | Unit::Svi | Unit::Lvw | Unit::Lvi | Unit::Dvw | Unit::Dvi => LengthUnit::Vw,
        Unit::Vh | Unit::Vb | Unit::Svh | Unit::Svb | Unit::Lvh | Unit::Lvb | Unit::Dvh | Unit::Dvb => LengthUnit::Vh,
        Unit::Vmin | Unit::Svmin | Unit::Lvmin | Unit::Dvmin => LengthUnit::Vmin,
        Unit::Vmax | Unit::Svmax | Unit::Lvmax | Unit::Dvmax => LengthUnit::Vmax,
        Unit::Cqw | Unit::Cqi => LengthUnit::Cqw,
        Unit::Cqh | Unit::Cqb => LengthUnit::Cqh,
        Unit::Cqmin => LengthUnit::Cqmin,
        Unit::Cqmax => LengthUnit::Cqmax,
        _ => return None,
    };
    Some(Length { value: value as f32, unit }.to_px(ctx))
}

/// Parse and reduce one math function, every relative unit resolved with `ctx`. `None` for a function that does
/// not parse, is not a `<length-percentage>` (`calc(10px * 2px)`, `sin(1)`), or names a unit `ctx` cannot resolve.
pub fn reduce(text: &str, ctx: &ResolveContext) -> Option<Node> {
    let e = calc::parse(text)?;
    if !calc::LENGTH_PERCENTAGE.contains(&e.category()) {
        return None;
    }
    let unknown = std::cell::Cell::new(false);
    let px = |v: f64, u: Unit| match relative_px(v, u, ctx) {
        Some(p) => f64::from(p),
        None => {
            unknown.set(true);
            0.0
        }
    };
    let resolved = e.absolutized(&px)?;
    if unknown.get() {
        return None;
    }
    Some(node(&resolved))
}

/// A computed length as Blink clamps a math function's (`CSSValueClampingUtils::ClampLength`): NaN is 0, an infinity
/// the largest finite value -- which layout then saturates (Chrome 150: `calc(infinity * 1px)` lays out
/// `3.35544e+07px`, `calc(NaN * 1px)` `0px`).
fn clamp_length(v: f64) -> f32 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(-(f32::MAX as f64), f32::MAX as f64) as f32
    }
}

/// The layout form of an absolutized calculation.
fn node(e: &Expr) -> Node {
    if let Some((pct, px)) = e.linear() {
        return Node::linear(clamp_length(pct), clamp_length(px));
    }
    if let Some((op, args)) = e.comparison() {
        let mut nodes: Vec<Node> = args.iter().map(node).collect();
        match op {
            Op::Min => return Node::Min(nodes),
            Op::Max => return Node::Max(nodes),
            Op::Clamp if nodes.len() == 3 => {
                let c = nodes.pop();
                let b = nodes.pop();
                let a = nodes.pop();
                if let (Some(a), Some(b), Some(c)) = (a, b, c) {
                    return Node::Clamp(Box::new(a), Box::new(b), Box::new(c));
                }
            }
            _ => {}
        }
    }
    Node::Calc(Box::new(e.clone()))
}

// ---- layout's side: a stable pointer per distinct expression ---------------------------------------------------

thread_local! {
    /// Every reduced expression layout has handed taffy, by its canonical text. taffy keeps a `calc()` as a raw
    /// pointer inside its `Style`, so the node must outlive every layout that uses it; interning by text bounds
    /// the store by the page's distinct values and never frees a node a `Style` may still point at.
    static INTERNED: std::cell::RefCell<std::collections::HashMap<String, Box<Node>>> = Default::default();
}

/// The stable address of `node` for taffy's calc hook (see [`resolve_ptr`]).
pub fn intern(node: Node) -> *const () {
    let key = node.serialize();
    INTERNED.with(|m| {
        let mut m = m.borrow_mut();
        let b = m.entry(key).or_insert_with(|| Box::new(node));
        (&**b as *const Node).cast::<()>()
    })
}

/// taffy's `resolve_calc_value`: the node at `ptr` (from [`intern`]) against `basis`.
pub fn resolve_ptr(ptr: *const (), basis: f32) -> f32 {
    if ptr.is_null() {
        return 0.0;
    }
    // SAFETY: every pointer taffy is given comes from `intern`, whose boxes live for the thread and are never
    // removed or moved (a `Box`'s heap address is stable while the map rehashes).
    unsafe { &*ptr.cast::<Node>() }.eval(basis)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ResolveContext {
        ResolveContext::new(10.0, 16.0, 1920.0, 991.0)
    }

    #[test]
    fn chrome_150s_values_measured_in_c8_repro_vhcalc() {
        let r = |t: &str| reduce(t, &ctx()).unwrap();
        assert_eq!(r("calc(100px + 10vh)").px(), Some(199.1));
        assert_eq!(r("max(100vh,700px)").px(), Some(991.0));
        assert_eq!(r("min(20vh, 300px)").px().map(f32::round), Some(198.0));
        assert_eq!(r("clamp(100px,30vh,2000px)").px(), Some(297.3));
        assert_eq!(r("calc(10vw + 5px)").px(), Some(197.0));
        assert_eq!(r("calc(1px + 2px)").px(), Some(3.0));
        assert_eq!(r("calc(100% - 20px)").eval(1000.0), 980.0);
        assert_eq!(r("calc(50% + 2em)").eval(1000.0), 520.0);
        assert_eq!(r("min(100%, 300px)").eval(1000.0), 300.0);
        assert_eq!(r("calc(10% - 5px)").eval(1000.0), 95.0);
    }

    #[test]
    fn nesting_products_and_serialization() {
        let r = |t: &str| reduce(t, &ctx()).unwrap();
        assert_eq!(r("calc(2 * (10px + 5%))").serialize(), "calc(10% + 20px)");
        assert_eq!(r("calc(100% / 3)").serialize(), "33.333%");
        assert_eq!(r("calc(100% - 20px)").serialize(), "calc(100% - 20px)");
        assert_eq!(r("min(100%, calc(50% + 10em))").serialize(), "min(100%, 50% + 100px)");
        assert_eq!(r("calc(min(10px, 5px) + 1px)").px(), Some(6.0));
        assert!(reduce("calc(10px * 2px)", &ctx()).is_none());
        assert!(reduce("calc(10px + 2)", &ctx()).is_none());
        assert!(reduce("calc(10px) x", &ctx()).is_none());
    }

    /// The functions this file did not parse before `style::calc`, against Chrome 150's widths of
    /// a block in a 1000px box (`fixtures/css-answers/calc-chrome-150.json`).
    #[test]
    fn every_math_function_reduces_as_chrome_150_lays_it_out() {
        let r = |t: &str| reduce(t, &ctx()).unwrap();
        assert_eq!(r("round(50%, 7px)").eval(1000.0), 497.0);
        assert_eq!(r("mod(50%, 7px)").eval(1000.0), 3.0);
        assert_eq!(r("abs(50% - 600px)").eval(1000.0), 100.0);
        assert_eq!(r("calc(10px * sign(50% - 10px))").eval(1000.0), 10.0);
        assert_eq!(r("calc(1px * pow(2, 3))").px(), Some(8.0));
        assert_eq!(r("calc(min(10%, 20px) + 5px)").eval(1000.0), 25.0);
        assert!(reduce("calc(1px * atan2(1, 1))", &ctx()).is_none());
        assert!(reduce("sin(1)", &ctx()).is_none());
    }

    #[test]
    fn an_interned_node_evaluates_through_its_pointer() {
        let p = intern(reduce("calc(100% - 20px)", &ctx()).unwrap());
        assert_eq!(resolve_ptr(p, 500.0), 480.0);
        assert_eq!(p, intern(reduce("calc(100% - 20px)", &ctx()).unwrap()));
    }
}
