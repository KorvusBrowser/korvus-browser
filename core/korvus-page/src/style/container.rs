//! `@container` conditions (CSS Containment 3 §6).
//!
//! A container query is asked of an ANCESTOR's box, so unlike `@media` its answer is not known until
//! that ancestor has been laid out. The query is parsed once, here, into a tree the cascade evaluates
//! against a [`QueryContainer`] the page supplies: the nearest ancestor whose `container-type` and
//! `container-name` fit the query, with its content-box size from the last layout and its computed
//! custom properties for `style()`.
//!
//! Measured on `/usr/bin/google-chrome` 150.0.7871.46 over loopback (the unit's `atrules.html`):
//!
//! ```text
//! .cq { container-type: inline-size; width: 300px }
//! @container (min-width: 200px) { .cqc { color: rgb(9, 9, 9) } }      rgb(9, 9, 9)
//! @container (min-width: 400px) { .cqx { color: … } }                   rgb(0, 0, 0)
//! .cqn { container: card / inline-size; width: 150px }
//! @container card (width < 200px) { .named { color: rgb(19, 19, 19) } } rgb(19, 19, 19)
//! <div style="--t: 1"> @container style(--t: 1) { .sty { … } }        rgb(29, 29, 29)
//! conditionText "card (width < 200px)"  containerName "card"  containerQuery "(width < 200px)"
//! ```
//!
//! Evaluation is three-valued, as the spec's is: a feature the container cannot answer (a `height`
//! query against an `inline-size` container, a feature this engine does not know) is UNKNOWN, `not`
//! of unknown is unknown, and a condition that ends unknown does not apply.

use cssparser::{Parser, ParserInput, Token};

/// A parsed `@container` prelude.
#[derive(Debug, Clone, PartialEq)]
pub struct ContainerCondition {
    /// The `<container-name>`, when one was written.
    pub name: Option<String>,
    pub query: Query,
}

/// `<container-query>`.
#[derive(Debug, Clone, PartialEq)]
pub enum Query {
    Not(Box<Query>),
    And(Vec<Query>),
    Or(Vec<Query>),
    Size(SizeFeature),
    Style(Box<StyleQuery>),
    /// `<general-enclosed>`: valid, always unknown.
    Unknown,
}

/// What a set of `@container` conditions can read of a container's own style ([`ContainerReads::of`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContainerReads {
    /// A condition names its container: `container-name` picks which ancestor answers.
    pub named: bool,
    /// A size feature: answered by a size container (`container-type` other than `normal`), its `em` by
    /// that container's font size.
    pub size: bool,
    /// The custom properties a `style()` query asks, sorted: the only part of a container's style one reads
    /// (`StyleQuery::Custom`; a standard property is unknown, as in Chrome 150).
    pub custom: Vec<String>,
}

impl ContainerReads {
    pub fn of<'a>(conditions: impl IntoIterator<Item = &'a ContainerCondition>) -> ContainerReads {
        fn style(q: &StyleQuery, out: &mut ContainerReads) {
            match q {
                StyleQuery::Not(q) => style(q, out),
                StyleQuery::And(qs) | StyleQuery::Or(qs) => qs.iter().for_each(|q| style(q, out)),
                StyleQuery::Custom(name, _) => out.custom.push(name.clone()),
                StyleQuery::Unknown => {}
            }
        }
        fn walk(q: &Query, out: &mut ContainerReads) {
            match q {
                Query::Not(q) => walk(q, out),
                Query::And(qs) | Query::Or(qs) => qs.iter().for_each(|q| walk(q, out)),
                Query::Size(_) => out.size = true,
                Query::Style(s) => style(s, out),
                Query::Unknown => {}
            }
        }
        let mut out = ContainerReads::default();
        for c in conditions {
            out.named |= c.name.is_some();
            walk(&c.query, &mut out);
        }
        out.custom.sort_unstable();
        out.custom.dedup();
        out
    }
}

/// `style( <style-query> )`.
#[derive(Debug, Clone, PartialEq)]
pub enum StyleQuery {
    Not(Box<StyleQuery>),
    And(Vec<StyleQuery>),
    Or(Vec<StyleQuery>),
    /// `--name: value`, or `--name` alone (`None`: "has a value").
    Custom(String, Option<String>),
    /// A standard property in a style query: Chrome 150 evaluates only custom properties.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Axis {
    Width,
    Height,
    InlineSize,
    BlockSize,
    AspectRatio,
    Orientation,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cmp {
    Lt,
    Le,
    Eq,
    Ge,
    Gt,
}

impl Cmp {
    fn holds(self, a: f32, b: f32) -> bool {
        match self {
            Cmp::Lt => a < b,
            Cmp::Le => a <= b,
            Cmp::Eq => (a - b).abs() < 1e-4,
            Cmp::Ge => a >= b,
            Cmp::Gt => a > b,
        }
    }
    fn flip(self) -> Cmp {
        match self {
            Cmp::Lt => Cmp::Gt,
            Cmp::Le => Cmp::Ge,
            Cmp::Gt => Cmp::Lt,
            Cmp::Ge => Cmp::Le,
            Cmp::Eq => Cmp::Eq,
        }
    }
}

/// A size feature: `(axis)` alone, or one or two comparisons against a value.
#[derive(Debug, Clone, PartialEq)]
pub struct SizeFeature {
    pub axis: Axis,
    /// `(width >= 10px)` is `[(Ge, 10)]`; `(10px < width < 20px)` is `[(Gt, 10), (Lt, 20)]`, each
    /// comparison written with the FEATURE on the left. Empty is the boolean form.
    pub tests: Vec<(Cmp, Value)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// A length, in the unit it was written in.
    Length(f32, String),
    Number(f32),
    Ratio(f32),
    Ident(String),
}

/// What a query is asked about: the container the cascade picked for it.
pub struct QueryContainer<'a> {
    /// The content-box size from the last layout; `None` before the element has been laid out.
    pub size: Option<(f32, f32)>,
    /// Which axes `container-type` makes queryable: `(inline, block)`.
    pub axes: (bool, bool),
    /// The container's font size in px, for `em`.
    pub font_size: f32,
    pub root_font_size: f32,
    pub viewport: (f32, f32),
    /// A computed custom property of the container.
    pub custom: &'a dyn Fn(&str) -> Option<String>,
}

impl ContainerCondition {
    /// Parse an `@container` prelude. `None` is an invalid prelude, which drops the rule.
    pub fn parse(prelude: &str) -> Option<ContainerCondition> {
        let mut input = ParserInput::new(prelude);
        let mut p = Parser::new(&mut input);
        let name = p
            .try_parse(|p| -> Result<String, cssparser::ParseError<'_, ()>> {
                let ident = p.expect_ident()?.to_string();
                let lower = ident.to_ascii_lowercase();
                if matches!(lower.as_str(), "not" | "and" | "or" | "none" | "initial" | "inherit" | "unset" | "default" | "revert" | "revert-layer") {
                    return Err(p.new_custom_error(()));
                }
                Ok(ident)
            })
            .ok();
        let query = if name.is_some() && p.is_exhausted() {
            // `@container card { … }` -- a name alone queries nothing and always matches its container.
            Query::And(Vec::new())
        } else {
            container_query(&mut p).ok()?
        };
        p.expect_exhausted().ok()?;
        Some(ContainerCondition { name, query })
    }

    /// The axes a container must make queryable for this condition, `(inline, block)`. A container
    /// whose `container-type` provides neither can still answer a pure `style()` query.
    pub fn needs(&self) -> (bool, bool) {
        fn walk(q: &Query, out: &mut (bool, bool)) {
            match q {
                Query::Not(q) => walk(q, out),
                Query::And(qs) | Query::Or(qs) => qs.iter().for_each(|q| walk(q, out)),
                Query::Size(f) => match f.axis {
                    Axis::Width | Axis::InlineSize => out.0 = true,
                    Axis::Height | Axis::BlockSize => out.1 = true,
                    Axis::AspectRatio | Axis::Orientation => {
                        out.0 = true;
                        out.1 = true;
                    }
                },
                Query::Style(_) | Query::Unknown => {}
            }
        }
        let mut out = (false, false);
        walk(&self.query, &mut out);
        out
    }

    /// Does the condition hold for `container`? Unknown is `false`.
    pub fn evaluate(&self, container: &QueryContainer) -> bool {
        eval(&self.query, container) == Some(true)
    }
}

fn container_query<'i>(p: &mut Parser<'i, '_>) -> Result<Query, cssparser::ParseError<'i, ()>> {
    if p.try_parse(|p| p.expect_ident_matching("not")).is_ok() {
        return Ok(Query::Not(Box::new(query_in_parens(p)?)));
    }
    let first = query_in_parens(p)?;
    let mut items = vec![first];
    let mut op: Option<bool> = None;
    loop {
        let next = p.try_parse(|p| -> Result<bool, cssparser::ParseError<'_, ()>> {
            let t = p.expect_ident()?.clone();
            if t.eq_ignore_ascii_case("and") {
                Ok(true)
            } else if t.eq_ignore_ascii_case("or") {
                Ok(false)
            } else {
                Err(p.new_custom_error(()))
            }
        });
        let Ok(is_and) = next else { break };
        if op.is_some_and(|o| o != is_and) {
            return Err(p.new_custom_error(()));
        }
        op = Some(is_and);
        items.push(query_in_parens(p)?);
    }
    Ok(match op {
        None => items.pop().unwrap_or(Query::Unknown),
        Some(true) => Query::And(items),
        Some(false) => Query::Or(items),
    })
}

fn query_in_parens<'i>(p: &mut Parser<'i, '_>) -> Result<Query, cssparser::ParseError<'i, ()>> {
    let token = p.next()?.clone();
    match token {
        Token::ParenthesisBlock => p.parse_nested_block(|p| {
            if let Ok(q) = p.try_parse(|p| {
                let q = container_query(p)?;
                p.expect_exhausted()?;
                Ok::<_, cssparser::ParseError<'_, ()>>(q)
            }) {
                return Ok(q);
            }
            if let Ok(f) = p.try_parse(size_feature) {
                return Ok(Query::Size(f));
            }
            while p.next().is_ok() {}
            Ok(Query::Unknown)
        }),
        Token::Function(ref name) if name.eq_ignore_ascii_case("style") => p.parse_nested_block(|p| {
            let q = style_query(p)?;
            p.expect_exhausted()?;
            Ok(Query::Style(Box::new(q)))
        }),
        Token::Function(_) => p.parse_nested_block(|p| {
            while p.next().is_ok() {}
            Ok(Query::Unknown)
        }),
        _ => Err(p.new_custom_error(())),
    }
}

fn style_query<'i>(p: &mut Parser<'i, '_>) -> Result<StyleQuery, cssparser::ParseError<'i, ()>> {
    if p.try_parse(|p| p.expect_ident_matching("not")).is_ok() {
        return Ok(StyleQuery::Not(Box::new(style_in_parens(p)?)));
    }
    // A bare declaration is the common form: `style(--t: 1)`.
    if let Ok(d) = p.try_parse(|p| {
        let d = style_feature(p)?;
        p.expect_exhausted()?;
        Ok::<_, cssparser::ParseError<'_, ()>>(d)
    }) {
        return Ok(d);
    }
    let mut items = vec![style_in_parens(p)?];
    let mut op: Option<bool> = None;
    loop {
        let next = p.try_parse(|p| -> Result<bool, cssparser::ParseError<'_, ()>> {
            let t = p.expect_ident()?.clone();
            if t.eq_ignore_ascii_case("and") {
                Ok(true)
            } else if t.eq_ignore_ascii_case("or") {
                Ok(false)
            } else {
                Err(p.new_custom_error(()))
            }
        });
        let Ok(is_and) = next else { break };
        if op.is_some_and(|o| o != is_and) {
            return Err(p.new_custom_error(()));
        }
        op = Some(is_and);
        items.push(style_in_parens(p)?);
    }
    Ok(match op {
        None => items.pop().unwrap_or(StyleQuery::Unknown),
        Some(true) => StyleQuery::And(items),
        Some(false) => StyleQuery::Or(items),
    })
}

fn style_in_parens<'i>(p: &mut Parser<'i, '_>) -> Result<StyleQuery, cssparser::ParseError<'i, ()>> {
    p.expect_parenthesis_block()?;
    p.parse_nested_block(|p| {
        if let Ok(q) = p.try_parse(|p| {
            let q = style_query(p)?;
            p.expect_exhausted()?;
            Ok::<_, cssparser::ParseError<'_, ()>>(q)
        }) {
            return Ok(q);
        }
        while p.next().is_ok() {}
        Ok(StyleQuery::Unknown)
    })
}

fn style_feature<'i>(p: &mut Parser<'i, '_>) -> Result<StyleQuery, cssparser::ParseError<'i, ()>> {
    let name = p.expect_ident()?.to_string();
    if p.is_exhausted() {
        return Ok(if name.starts_with("--") { StyleQuery::Custom(name, None) } else { StyleQuery::Unknown });
    }
    p.expect_colon()?;
    p.skip_whitespace();
    let start = p.position();
    while p.next().is_ok() {}
    let value = p.slice_from(start).trim().to_string();
    Ok(if name.starts_with("--") { StyleQuery::Custom(name, Some(value)) } else { StyleQuery::Unknown })
}

fn axis_of(name: &str) -> Option<Axis> {
    Some(match name.to_ascii_lowercase().as_str() {
        "width" => Axis::Width,
        "height" => Axis::Height,
        "inline-size" => Axis::InlineSize,
        "block-size" => Axis::BlockSize,
        "aspect-ratio" => Axis::AspectRatio,
        "orientation" => Axis::Orientation,
        _ => return None,
    })
}

fn comparator<'i>(p: &mut Parser<'i, '_>) -> Result<Cmp, cssparser::ParseError<'i, ()>> {
    let first = match p.next()? {
        Token::Delim('<') => Cmp::Lt,
        Token::Delim('>') => Cmp::Gt,
        Token::Delim('=') => return Ok(Cmp::Eq),
        _ => return Err(p.new_custom_error(())),
    };
    if p.try_parse(|p| p.expect_delim('=')).is_ok() {
        return Ok(if first == Cmp::Lt { Cmp::Le } else { Cmp::Ge });
    }
    Ok(first)
}

fn value<'i>(p: &mut Parser<'i, '_>) -> Result<Value, cssparser::ParseError<'i, ()>> {
    let t = p.next()?.clone();
    match t {
        Token::Dimension { value, unit, .. } => Ok(Value::Length(value, unit.to_ascii_lowercase())),
        Token::Number { value, .. } => {
            if p.try_parse(|p| p.expect_delim('/')).is_ok() {
                let d = p.expect_number()?;
                return Ok(Value::Ratio(if d == 0.0 { f32::INFINITY } else { value / d }));
            }
            Ok(Value::Number(value))
        }
        Token::Ident(i) => Ok(Value::Ident(i.to_ascii_lowercase())),
        _ => Err(p.new_custom_error(())),
    }
}

fn size_feature<'i>(p: &mut Parser<'i, '_>) -> Result<SizeFeature, cssparser::ParseError<'i, ()>> {
    // `(value op axis [op value])`
    if let Ok(f) = p.try_parse(|p| {
        let v1 = value(p)?;
        let c1 = comparator(p)?;
        let axis = axis_of(p.expect_ident()?).ok_or_else(|| p.new_custom_error(()))?;
        let mut tests = vec![(c1.flip(), v1)];
        if let Ok((c2, v2)) = p.try_parse(|p| Ok::<_, cssparser::ParseError<'_, ()>>((comparator(p)?, value(p)?))) {
            tests.push((c2, v2));
        }
        p.expect_exhausted()?;
        Ok::<_, cssparser::ParseError<'_, ()>>(SizeFeature { axis, tests })
    }) {
        return Ok(f);
    }
    let name = p.expect_ident()?.to_string();
    let lower = name.to_ascii_lowercase();
    if p.is_exhausted() {
        let axis = axis_of(&lower).ok_or_else(|| p.new_custom_error(()))?;
        return Ok(SizeFeature { axis, tests: Vec::new() });
    }
    if p.try_parse(|p| p.expect_colon()).is_ok() {
        let (cmp, bare) = if let Some(rest) = lower.strip_prefix("min-") {
            (Cmp::Ge, rest.to_string())
        } else if let Some(rest) = lower.strip_prefix("max-") {
            (Cmp::Le, rest.to_string())
        } else {
            (Cmp::Eq, lower.clone())
        };
        let axis = axis_of(&bare).ok_or_else(|| p.new_custom_error(()))?;
        let v = value(p)?;
        p.expect_exhausted()?;
        return Ok(SizeFeature { axis, tests: vec![(cmp, v)] });
    }
    let axis = axis_of(&lower).ok_or_else(|| p.new_custom_error(()))?;
    let c = comparator(p)?;
    let v = value(p)?;
    p.expect_exhausted()?;
    Ok(SizeFeature { axis, tests: vec![(c, v)] })
}

fn to_px(v: &Value, c: &QueryContainer) -> Option<f32> {
    match v {
        Value::Length(n, unit) => Some(match unit.as_str() {
            "px" => *n,
            "em" => n * c.font_size,
            "rem" => n * c.root_font_size,
            "vw" => n * c.viewport.0 / 100.0,
            "vh" => n * c.viewport.1 / 100.0,
            "vmin" => n * c.viewport.0.min(c.viewport.1) / 100.0,
            "vmax" => n * c.viewport.0.max(c.viewport.1) / 100.0,
            "in" => n * 96.0,
            "cm" => n * 96.0 / 2.54,
            "mm" => n * 96.0 / 25.4,
            "pt" => n * 96.0 / 72.0,
            "pc" => n * 16.0,
            _ => return None,
        }),
        Value::Number(n) if *n == 0.0 => Some(0.0),
        _ => None,
    }
}

fn eval(q: &Query, c: &QueryContainer) -> Option<bool> {
    match q {
        Query::Not(inner) => eval(inner, c).map(|b| !b),
        Query::And(items) => {
            let mut out = Some(true);
            for i in items {
                match eval(i, c) {
                    Some(false) => return Some(false),
                    None => out = None,
                    Some(true) => {}
                }
            }
            out
        }
        Query::Or(items) => {
            let mut out = Some(false);
            for i in items {
                match eval(i, c) {
                    Some(true) => return Some(true),
                    None => out = None,
                    Some(false) => {}
                }
            }
            out
        }
        Query::Unknown => None,
        Query::Style(s) => eval_style(s, c),
        Query::Size(f) => {
            let (w, h) = c.size?;
            let (inline, block) = c.axes;
            let actual = match f.axis {
                Axis::Width | Axis::InlineSize if inline => w,
                Axis::Height | Axis::BlockSize if block => h,
                Axis::AspectRatio if inline && block => {
                    if h == 0.0 {
                        return None;
                    }
                    w / h
                }
                Axis::Orientation if inline && block => {
                    return match f.tests.first() {
                        Some((Cmp::Eq, Value::Ident(i))) if i == "portrait" => Some(h >= w),
                        Some((Cmp::Eq, Value::Ident(i))) if i == "landscape" => Some(w > h),
                        _ => None,
                    };
                }
                _ => return None,
            };
            if f.tests.is_empty() {
                return Some(actual != 0.0);
            }
            for (cmp, v) in &f.tests {
                let rhs = match (f.axis, v) {
                    (Axis::AspectRatio, Value::Ratio(r)) => *r,
                    (Axis::AspectRatio, Value::Number(n)) => *n,
                    (_, v) => to_px(v, c)?,
                };
                if !cmp.holds(actual, rhs) {
                    return Some(false);
                }
            }
            Some(true)
        }
    }
}

fn eval_style(q: &StyleQuery, c: &QueryContainer) -> Option<bool> {
    match q {
        StyleQuery::Not(inner) => eval_style(inner, c).map(|b| !b),
        StyleQuery::And(items) => {
            let mut out = Some(true);
            for i in items {
                match eval_style(i, c) {
                    Some(false) => return Some(false),
                    None => out = None,
                    Some(true) => {}
                }
            }
            out
        }
        StyleQuery::Or(items) => {
            let mut out = Some(false);
            for i in items {
                match eval_style(i, c) {
                    Some(true) => return Some(true),
                    None => out = None,
                    Some(false) => {}
                }
            }
            out
        }
        StyleQuery::Unknown => None,
        StyleQuery::Custom(name, wanted) => {
            let have = (c.custom)(name);
            Some(match wanted {
                None => have.is_some_and(|v| !v.trim().is_empty()),
                Some(w) => have.is_some_and(|v| normalize(&v) == normalize(w)),
            })
        }
    }
}

/// Token-level equality for a custom property value: whitespace runs collapse.
fn normalize(v: &str) -> String {
    v.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
}

/// `conditionText` / `containerQuery` spelling: whitespace runs collapsed.
pub fn collapse(text: &str) -> String {
    text.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(w: f32, h: f32, axes: (bool, bool)) -> QueryContainer<'static> {
        QueryContainer { size: Some((w, h)), axes, font_size: 16.0, root_font_size: 16.0, viewport: (800.0, 600.0), custom: &|_| None }
    }

    #[test]
    fn size_features_in_all_three_spellings() {
        let c = at(300.0, 50.0, (true, false));
        assert!(ContainerCondition::parse("(min-width: 200px)").unwrap().evaluate(&c));
        assert!(!ContainerCondition::parse("(min-width: 400px)").unwrap().evaluate(&c));
        assert!(ContainerCondition::parse("(width > 250px)").unwrap().evaluate(&c));
        assert!(ContainerCondition::parse("(200px < width <= 300px)").unwrap().evaluate(&c));
        assert!(ContainerCondition::parse("(width > 10em)").unwrap().evaluate(&c));
        // An inline-size container cannot answer a block-axis question.
        assert!(!ContainerCondition::parse("(height > 1px)").unwrap().evaluate(&c));
        assert!(!ContainerCondition::parse("not (height > 1px)").unwrap().evaluate(&c));
    }

    #[test]
    fn a_name_is_split_from_the_query() {
        let c = ContainerCondition::parse("card (width < 200px)").unwrap();
        assert_eq!(c.name.as_deref(), Some("card"));
        assert!(c.evaluate(&at(150.0, 0.0, (true, false))));
        assert!(ContainerCondition::parse("not (width < 1px)").unwrap().name.is_none());
    }

    #[test]
    fn style_queries_read_the_containers_custom_properties() {
        let custom = |n: &str| (n == "--t").then(|| " 1".to_string());
        let c = QueryContainer { size: None, axes: (false, false), font_size: 16.0, root_font_size: 16.0, viewport: (0.0, 0.0), custom: &custom };
        assert!(ContainerCondition::parse("style(--t: 1)").unwrap().evaluate(&c));
        assert!(!ContainerCondition::parse("style(--t: 2)").unwrap().evaluate(&c));
        assert!(ContainerCondition::parse("style(--t)").unwrap().evaluate(&c));
        assert!(ContainerCondition::parse("style(not (--t: 2))").unwrap().evaluate(&c));
    }
}
