//! `core/xml/xpath_parse.rs` -- the XPath 1.0 lexer and parser behind `XPathEvaluator`,
//! `XPathExpression` and `document.evaluate` (L14 cause C09).
//!
//! Pure Rust, no v8: a string and a namespace-prefix resolver in, an [`Expr`] out. The grammar is
//! XPath 1.0 §3 with the lexical disambiguation rules of §3.7, and the function library is §4's,
//! with arity checked here -- Chrome 150 rejects `foo()` and `concat("a")` as "not a valid XPath
//! expression" at parse time, not at evaluation. Prefixes are resolved while parsing (Blink resolves
//! them in its parser too, which is why an unresolvable prefix is a NamespaceError from
//! `createExpression` rather than from `evaluate`).

/// The thirteen axes of XPath 1.0 §2.2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Axis {
    Ancestor,
    AncestorOrSelf,
    Attribute,
    Child,
    Descendant,
    DescendantOrSelf,
    Following,
    FollowingSibling,
    Namespace,
    Parent,
    Preceding,
    PrecedingSibling,
    SelfAxis,
}

impl Axis {
    fn from_name(n: &str) -> Option<Axis> {
        Some(match n {
            "ancestor" => Axis::Ancestor,
            "ancestor-or-self" => Axis::AncestorOrSelf,
            "attribute" => Axis::Attribute,
            "child" => Axis::Child,
            "descendant" => Axis::Descendant,
            "descendant-or-self" => Axis::DescendantOrSelf,
            "following" => Axis::Following,
            "following-sibling" => Axis::FollowingSibling,
            "namespace" => Axis::Namespace,
            "parent" => Axis::Parent,
            "preceding" => Axis::Preceding,
            "preceding-sibling" => Axis::PrecedingSibling,
            "self" => Axis::SelfAxis,
            _ => return None,
        })
    }
}

/// A node test. `Name.local == "*"` is the wildcard; `ns` is the RESOLVED namespace URI of the
/// prefix (`None` for an unprefixed test).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum NodeTest {
    AnyNode,
    Text,
    Comment,
    Pi(Option<String>),
    Name { ns: Option<String>, local: String },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Step {
    pub axis: Axis,
    pub test: NodeTest,
    pub preds: Vec<Expr>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RelOp {
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

/// The core function library (§4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Func {
    Last,
    Position,
    Count,
    Id,
    LocalName,
    NamespaceUri,
    Name,
    String,
    Concat,
    StartsWith,
    Contains,
    SubstringBefore,
    SubstringAfter,
    Substring,
    StringLength,
    NormalizeSpace,
    Translate,
    Boolean,
    Not,
    True,
    False,
    Lang,
    Number,
    Sum,
    Floor,
    Ceiling,
    Round,
}

impl Func {
    /// The function and its (min, max) argument count; `usize::MAX` is unbounded.
    fn lookup(name: &str) -> Option<(Func, usize, usize)> {
        Some(match name {
            "last" => (Func::Last, 0, 0),
            "position" => (Func::Position, 0, 0),
            "count" => (Func::Count, 1, 1),
            "id" => (Func::Id, 1, 1),
            "local-name" => (Func::LocalName, 0, 1),
            "namespace-uri" => (Func::NamespaceUri, 0, 1),
            "name" => (Func::Name, 0, 1),
            "string" => (Func::String, 0, 1),
            "concat" => (Func::Concat, 2, usize::MAX),
            "starts-with" => (Func::StartsWith, 2, 2),
            "contains" => (Func::Contains, 2, 2),
            "substring-before" => (Func::SubstringBefore, 2, 2),
            "substring-after" => (Func::SubstringAfter, 2, 2),
            "substring" => (Func::Substring, 2, 3),
            "string-length" => (Func::StringLength, 0, 1),
            "normalize-space" => (Func::NormalizeSpace, 0, 1),
            "translate" => (Func::Translate, 3, 3),
            "boolean" => (Func::Boolean, 1, 1),
            "not" => (Func::Not, 1, 1),
            "true" => (Func::True, 0, 0),
            "false" => (Func::False, 0, 0),
            "lang" => (Func::Lang, 1, 1),
            "number" => (Func::Number, 0, 1),
            "sum" => (Func::Sum, 1, 1),
            "floor" => (Func::Floor, 1, 1),
            "ceiling" => (Func::Ceiling, 1, 1),
            "round" => (Func::Round, 1, 1),
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Expr {
    Or(Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    /// `=` (false) or `!=` (true).
    Eq(bool, Box<Expr>, Box<Expr>),
    Rel(RelOp, Box<Expr>, Box<Expr>),
    Arith(ArithOp, Box<Expr>, Box<Expr>),
    Neg(Box<Expr>),
    Union(Box<Expr>, Box<Expr>),
    /// A location path. `absolute` starts from the root of the context node's tree.
    Path { absolute: bool, steps: Vec<Step> },
    /// A filter expression, its predicates, and the relative path that may follow it.
    Filter { primary: Box<Expr>, preds: Vec<Expr>, steps: Vec<Step> },
    Literal(String),
    Number(f64),
    Var(String),
    Call(Func, Vec<Expr>),
}

/// Why a string is not an expression.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ParseError {
    /// "The string '…' is not a valid XPath expression." -- a SyntaxError.
    Syntax,
    /// "The string '…' contains unresolvable namespaces." -- a NamespaceError.
    Namespace,
    /// The resolver threw; its exception is already pending in the caller's scope.
    Thrown,
}

// ── the lexer ─────────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    LParen,
    RParen,
    LBracket,
    RBracket,
    Dot,
    DotDot,
    At,
    Comma,
    ColonColon,
    Slash,
    DSlash,
    Pipe,
    Plus,
    Minus,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Mod,
    Div,
    Mul,
    /// `*`, `prefix:*` or a QName in a name-test position.
    NameTest(Option<String>, String),
    NodeType(String),
    FuncName(String),
    AxisName(String),
    Literal(String),
    Number(f64),
    Var(String),
}

impl Tok {
    fn is_operator(&self) -> bool {
        matches!(
            self,
            Tok::And | Tok::Or | Tok::Mod | Tok::Div | Tok::Mul | Tok::Slash | Tok::DSlash | Tok::Pipe | Tok::Plus
                | Tok::Minus | Tok::Eq | Tok::Ne | Tok::Lt | Tok::Le | Tok::Gt | Tok::Ge
        )
    }
}

fn is_xml_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || (c as u32) >= 0x80
}

fn is_name_char(c: char) -> bool {
    is_name_start(c) || c.is_ascii_digit() || c == '-' || c == '.'
}

fn lex(src: &str) -> Result<Vec<Tok>, ParseError> {
    let c: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut out: Vec<Tok> = Vec::new();
    let read_ncname = |i: &mut usize| -> Option<String> {
        if *i < c.len() && is_name_start(c[*i]) {
            let s = *i;
            while *i < c.len() && is_name_char(c[*i]) {
                *i += 1;
            }
            Some(c[s..*i].iter().collect())
        } else {
            None
        }
    };
    while i < c.len() {
        let ch = c[i];
        if is_xml_ws(ch) {
            i += 1;
            continue;
        }
        // §3.7: after a token that can end an operand, `*` multiplies and an NCName is an operator.
        let operator_context = match out.last() {
            None => false,
            Some(t) => !(matches!(t, Tok::At | Tok::ColonColon | Tok::LParen | Tok::LBracket | Tok::Comma) || t.is_operator()),
        };
        let next = c.get(i + 1).copied();
        let tok = match ch {
            '(' => { i += 1; Tok::LParen }
            ')' => { i += 1; Tok::RParen }
            '[' => { i += 1; Tok::LBracket }
            ']' => { i += 1; Tok::RBracket }
            '@' => { i += 1; Tok::At }
            ',' => { i += 1; Tok::Comma }
            '|' => { i += 1; Tok::Pipe }
            '+' => { i += 1; Tok::Plus }
            '-' => { i += 1; Tok::Minus }
            '=' => { i += 1; Tok::Eq }
            '!' if next == Some('=') => { i += 2; Tok::Ne }
            '<' if next == Some('=') => { i += 2; Tok::Le }
            '<' => { i += 1; Tok::Lt }
            '>' if next == Some('=') => { i += 2; Tok::Ge }
            '>' => { i += 1; Tok::Gt }
            ':' if next == Some(':') => { i += 2; Tok::ColonColon }
            '/' if next == Some('/') => { i += 2; Tok::DSlash }
            '/' => { i += 1; Tok::Slash }
            '.' if next == Some('.') => { i += 2; Tok::DotDot }
            '.' if next.is_some_and(|d| d.is_ascii_digit()) => {
                let s = i;
                i += 1;
                while i < c.len() && c[i].is_ascii_digit() {
                    i += 1;
                }
                let text: String = c[s..i].iter().collect();
                Tok::Number(text.parse::<f64>().map_err(|_| ParseError::Syntax)?)
            }
            '.' => { i += 1; Tok::Dot }
            '"' | '\'' => {
                let q = ch;
                let s = i + 1;
                let mut j = s;
                while j < c.len() && c[j] != q {
                    j += 1;
                }
                if j >= c.len() {
                    return Err(ParseError::Syntax);
                }
                i = j + 1;
                Tok::Literal(c[s..j].iter().collect())
            }
            d if d.is_ascii_digit() => {
                let s = i;
                while i < c.len() && c[i].is_ascii_digit() {
                    i += 1;
                }
                if i < c.len() && c[i] == '.' {
                    i += 1;
                    while i < c.len() && c[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                let text: String = c[s..i].iter().collect();
                Tok::Number(text.parse::<f64>().map_err(|_| ParseError::Syntax)?)
            }
            '$' => {
                i += 1;
                let Some(first) = read_ncname(&mut i) else { return Err(ParseError::Syntax) };
                if i + 1 < c.len() && c[i] == ':' && is_name_start(c[i + 1]) {
                    i += 1;
                    let local = read_ncname(&mut i).ok_or(ParseError::Syntax)?;
                    Tok::Var(format!("{first}:{local}"))
                } else {
                    Tok::Var(first)
                }
            }
            '*' => {
                i += 1;
                if operator_context { Tok::Mul } else { Tok::NameTest(None, "*".into()) }
            }
            s if is_name_start(s) => {
                let first = read_ncname(&mut i).ok_or(ParseError::Syntax)?;
                if operator_context {
                    match first.as_str() {
                        "and" => Tok::And,
                        "or" => Tok::Or,
                        "mod" => Tok::Mod,
                        "div" => Tok::Div,
                        _ => return Err(ParseError::Syntax),
                    }
                } else {
                    // `prefix:*`, `prefix:local`, or a bare NCName.
                    let (prefix, local) = if i + 1 < c.len() && c[i] == ':' && c[i + 1] == '*' {
                        i += 2;
                        (Some(first), "*".to_string())
                    } else if i + 1 < c.len() && c[i] == ':' && is_name_start(c[i + 1]) {
                        i += 1;
                        let local = read_ncname(&mut i).ok_or(ParseError::Syntax)?;
                        (Some(first), local)
                    } else {
                        (None, first)
                    };
                    let mut j = i;
                    while j < c.len() && is_xml_ws(c[j]) {
                        j += 1;
                    }
                    if local != "*" && j < c.len() && c[j] == '(' {
                        let is_type = prefix.is_none() && matches!(local.as_str(), "comment" | "text" | "processing-instruction" | "node");
                        if is_type {
                            Tok::NodeType(local)
                        } else {
                            Tok::FuncName(match prefix {
                                Some(p) => format!("{p}:{local}"),
                                None => local,
                            })
                        }
                    } else if prefix.is_none() && local != "*" && j + 1 < c.len() && c[j] == ':' && c[j + 1] == ':' {
                        Tok::AxisName(local)
                    } else {
                        Tok::NameTest(prefix, local)
                    }
                }
            }
            _ => return Err(ParseError::Syntax),
        };
        out.push(tok);
    }
    Ok(out)
}

// ── the parser ────────────────────────────────────────────────────────────────────────────────────

struct Parser<'r> {
    toks: Vec<Tok>,
    pos: usize,
    resolve: &'r mut dyn FnMut(&str) -> Result<Option<String>, ()>,
}

/// Parse `src`, resolving every name-test prefix through `resolve` (`Ok(None)` = unresolvable,
/// `Err(())` = the resolver threw).
pub(crate) fn parse(src: &str, resolve: &mut dyn FnMut(&str) -> Result<Option<String>, ()>) -> Result<Expr, ParseError> {
    let toks = lex(src)?;
    let mut p = Parser { toks, pos: 0, resolve };
    let e = p.or_expr()?;
    if p.pos != p.toks.len() {
        return Err(ParseError::Syntax);
    }
    Ok(e)
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn eat(&mut self, t: &Tok) -> bool {
        if self.peek() == Some(t) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, t: &Tok) -> Result<(), ParseError> {
        if self.eat(t) { Ok(()) } else { Err(ParseError::Syntax) }
    }

    fn or_expr(&mut self) -> Result<Expr, ParseError> {
        let mut l = self.and_expr()?;
        while self.eat(&Tok::Or) {
            let r = self.and_expr()?;
            l = Expr::Or(Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn and_expr(&mut self) -> Result<Expr, ParseError> {
        let mut l = self.equality()?;
        while self.eat(&Tok::And) {
            let r = self.equality()?;
            l = Expr::And(Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn equality(&mut self) -> Result<Expr, ParseError> {
        let mut l = self.relational()?;
        loop {
            let ne = match self.peek() {
                Some(Tok::Eq) => false,
                Some(Tok::Ne) => true,
                _ => return Ok(l),
            };
            self.pos += 1;
            let r = self.relational()?;
            l = Expr::Eq(ne, Box::new(l), Box::new(r));
        }
    }

    fn relational(&mut self) -> Result<Expr, ParseError> {
        let mut l = self.additive()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Lt) => RelOp::Lt,
                Some(Tok::Le) => RelOp::Le,
                Some(Tok::Gt) => RelOp::Gt,
                Some(Tok::Ge) => RelOp::Ge,
                _ => return Ok(l),
            };
            self.pos += 1;
            let r = self.additive()?;
            l = Expr::Rel(op, Box::new(l), Box::new(r));
        }
    }

    fn additive(&mut self) -> Result<Expr, ParseError> {
        let mut l = self.multiplicative()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Plus) => ArithOp::Add,
                Some(Tok::Minus) => ArithOp::Sub,
                _ => return Ok(l),
            };
            self.pos += 1;
            let r = self.multiplicative()?;
            l = Expr::Arith(op, Box::new(l), Box::new(r));
        }
    }

    fn multiplicative(&mut self) -> Result<Expr, ParseError> {
        let mut l = self.unary()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Mul) => ArithOp::Mul,
                Some(Tok::Div) => ArithOp::Div,
                Some(Tok::Mod) => ArithOp::Mod,
                _ => return Ok(l),
            };
            self.pos += 1;
            let r = self.unary()?;
            l = Expr::Arith(op, Box::new(l), Box::new(r));
        }
    }

    fn unary(&mut self) -> Result<Expr, ParseError> {
        if self.eat(&Tok::Minus) {
            let e = self.unary()?;
            return Ok(Expr::Neg(Box::new(e)));
        }
        self.union()
    }

    fn union(&mut self) -> Result<Expr, ParseError> {
        let mut l = self.path_expr()?;
        while self.eat(&Tok::Pipe) {
            let r = self.path_expr()?;
            l = Expr::Union(Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn starts_step(&self) -> bool {
        matches!(
            self.peek(),
            Some(Tok::Dot | Tok::DotDot | Tok::At | Tok::AxisName(_) | Tok::NameTest(..) | Tok::NodeType(_))
        )
    }

    fn path_expr(&mut self) -> Result<Expr, ParseError> {
        match self.peek() {
            Some(Tok::Literal(_) | Tok::Number(_) | Tok::Var(_) | Tok::LParen | Tok::FuncName(_)) => {
                let primary = self.primary()?;
                let mut preds = Vec::new();
                while self.peek() == Some(&Tok::LBracket) {
                    preds.push(self.predicate()?);
                }
                let mut steps = Vec::new();
                match self.peek() {
                    Some(Tok::Slash) => {
                        self.pos += 1;
                        self.relative_path(&mut steps)?;
                    }
                    Some(Tok::DSlash) => {
                        self.pos += 1;
                        steps.push(descendant_or_self());
                        self.relative_path(&mut steps)?;
                    }
                    _ => {}
                }
                if preds.is_empty() && steps.is_empty() {
                    return Ok(primary);
                }
                Ok(Expr::Filter { primary: Box::new(primary), preds, steps })
            }
            Some(Tok::Slash) => {
                self.pos += 1;
                let mut steps = Vec::new();
                if self.starts_step() {
                    self.relative_path(&mut steps)?;
                }
                Ok(Expr::Path { absolute: true, steps })
            }
            Some(Tok::DSlash) => {
                self.pos += 1;
                let mut steps = vec![descendant_or_self()];
                self.relative_path(&mut steps)?;
                Ok(Expr::Path { absolute: true, steps })
            }
            _ if self.starts_step() => {
                let mut steps = Vec::new();
                self.relative_path(&mut steps)?;
                Ok(Expr::Path { absolute: false, steps })
            }
            _ => Err(ParseError::Syntax),
        }
    }

    fn relative_path(&mut self, steps: &mut Vec<Step>) -> Result<(), ParseError> {
        steps.push(self.step()?);
        loop {
            match self.peek() {
                Some(Tok::Slash) => {
                    self.pos += 1;
                    steps.push(self.step()?);
                }
                Some(Tok::DSlash) => {
                    self.pos += 1;
                    steps.push(descendant_or_self());
                    steps.push(self.step()?);
                }
                _ => return Ok(()),
            }
        }
    }

    fn step(&mut self) -> Result<Step, ParseError> {
        if self.eat(&Tok::Dot) {
            return Ok(Step { axis: Axis::SelfAxis, test: NodeTest::AnyNode, preds: Vec::new() });
        }
        if self.eat(&Tok::DotDot) {
            return Ok(Step { axis: Axis::Parent, test: NodeTest::AnyNode, preds: Vec::new() });
        }
        let axis = match self.peek().cloned() {
            Some(Tok::At) => {
                self.pos += 1;
                Axis::Attribute
            }
            Some(Tok::AxisName(n)) => {
                self.pos += 1;
                let a = Axis::from_name(&n).ok_or(ParseError::Syntax)?;
                self.expect(&Tok::ColonColon)?;
                a
            }
            _ => Axis::Child,
        };
        let test = match self.peek().cloned() {
            Some(Tok::NameTest(prefix, local)) => {
                self.pos += 1;
                let ns = match prefix {
                    None => None,
                    Some(p) => match (self.resolve)(&p) {
                        Ok(Some(uri)) => Some(uri),
                        Ok(None) => return Err(ParseError::Namespace),
                        Err(()) => return Err(ParseError::Thrown),
                    },
                };
                NodeTest::Name { ns, local }
            }
            Some(Tok::NodeType(t)) => {
                self.pos += 1;
                self.expect(&Tok::LParen)?;
                let test = match t.as_str() {
                    "node" => NodeTest::AnyNode,
                    "text" => NodeTest::Text,
                    "comment" => NodeTest::Comment,
                    _ => match self.peek().cloned() {
                        Some(Tok::Literal(l)) => {
                            self.pos += 1;
                            NodeTest::Pi(Some(l))
                        }
                        _ => NodeTest::Pi(None),
                    },
                };
                self.expect(&Tok::RParen)?;
                test
            }
            _ => return Err(ParseError::Syntax),
        };
        let mut preds = Vec::new();
        while self.peek() == Some(&Tok::LBracket) {
            preds.push(self.predicate()?);
        }
        Ok(Step { axis, test, preds })
    }

    fn predicate(&mut self) -> Result<Expr, ParseError> {
        self.expect(&Tok::LBracket)?;
        let e = self.or_expr()?;
        self.expect(&Tok::RBracket)?;
        Ok(e)
    }

    fn primary(&mut self) -> Result<Expr, ParseError> {
        match self.peek().cloned() {
            Some(Tok::Literal(s)) => {
                self.pos += 1;
                Ok(Expr::Literal(s))
            }
            Some(Tok::Number(n)) => {
                self.pos += 1;
                Ok(Expr::Number(n))
            }
            Some(Tok::Var(v)) => {
                self.pos += 1;
                Ok(Expr::Var(v))
            }
            Some(Tok::LParen) => {
                self.pos += 1;
                let e = self.or_expr()?;
                self.expect(&Tok::RParen)?;
                Ok(e)
            }
            Some(Tok::FuncName(name)) => {
                self.pos += 1;
                self.expect(&Tok::LParen)?;
                let mut args = Vec::new();
                if !self.eat(&Tok::RParen) {
                    loop {
                        args.push(self.or_expr()?);
                        if self.eat(&Tok::Comma) {
                            continue;
                        }
                        self.expect(&Tok::RParen)?;
                        break;
                    }
                }
                let (f, min, max) = Func::lookup(&name).ok_or(ParseError::Syntax)?;
                if args.len() < min || args.len() > max {
                    return Err(ParseError::Syntax);
                }
                Ok(Expr::Call(f, args))
            }
            _ => Err(ParseError::Syntax),
        }
    }
}

fn descendant_or_self() -> Step {
    Step { axis: Axis::DescendantOrSelf, test: NodeTest::AnyNode, preds: Vec::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<Expr, ParseError> {
        parse(s, &mut |pre: &str| Ok(if pre == "x" { Some("urn:x".to_string()) } else { None }))
    }

    #[test]
    fn chromes_invalid_expressions_are_syntax_errors() {
        for bad in ["//[", "1 +", "foo()", "concat(\"a\")", "", "//li[", "a/", "@", "child::", "bogus::a", "1e3", "processing-instruction(1)"] {
            assert_eq!(p(bad), Err(ParseError::Syntax), "{bad:?}");
        }
    }

    #[test]
    fn an_unresolvable_prefix_is_a_namespace_error() {
        assert_eq!(p("//y:li"), Err(ParseError::Namespace));
        assert!(p("//x:li").is_ok());
    }

    #[test]
    fn the_lexical_disambiguation_of_star_and_operator_names() {
        // `*` after an operand multiplies; `div` after an operand divides; both as names otherwise.
        assert!(matches!(p("2 * 3"), Ok(Expr::Arith(ArithOp::Mul, ..))));
        assert!(matches!(p("div"), Ok(Expr::Path { .. })));
        assert!(matches!(p("div div div"), Ok(Expr::Arith(ArithOp::Div, ..))));
        assert!(matches!(p("*"), Ok(Expr::Path { .. })));
        assert!(p("//li[@n > 1][last()]").is_ok());
        assert!(p("-(1) - -2").is_ok());
        assert!(p("(//li)[1]/@class | //ul").is_ok());
        assert!(p("processing-instruction('x')").is_ok());
        assert!(p("$x").is_ok());
        assert!(p(".5 + 5. + 1.25").is_ok());
    }
}
