//! Media queries: the conditions a block of rules is only sometimes in.
//!
//! # Why these are evaluated at match time and not at parse time
//!
//! Inlining the rules of a matching `@media` and discarding the rest would be simpler and, for a
//! scraper that loads a page once, indistinguishable. It is wrong for two reasons that both show up
//! as tells rather than as bugs: `document.styleSheets[0].cssRules` would show the inner rules
//! flattened where Chrome shows a `CSSMediaRule`, and a page that changes the viewport and reads
//! `getComputedStyle` again would see the old answer. So a rule keeps a reference to the condition
//! it sits under, and the condition is asked each time.
//!
//! # Blink 150's parser, evaluator and serializer
//!
//! This file was a subset -- MQ3 plus the range syntax, a fixed table of "desktop" features -- and the subset was
//! readable: BrowserScan and a fingerprinting library read `matchMedia` for `(inverted-colors: none)` (Chrome 150: false, the
//! feature is behind the experimental `InvertedColors`), `(video-dynamic-range: standard)` (false, experimental),
//! `(device-posture: continuous)`, `(horizontal-viewport-segments: 1)`, `(-webkit-transform-3d)` and `(grid: 0)`
//! (all true), and `.media` for the serialization (`(min-aspect-ratio: 16 / 9)`, `screen, not all`). It is now a port
//! of Chromium 150's:
//!
//! * `media_query_parser.cc` + `conditional_parser.cc`: `not`/`and`/`or` and nested parentheses, the range forms,
//!   and `<general-enclosed>` -- a parenthesised condition that is not a feature this build knows is kept as written
//!   and evaluates to UNKNOWN (Kleene logic: `not (foo)` is unknown too, and a list matches only on true);
//! * `media_query_exp.cc`: which features exist with which values -- `media_feature_names.json5` minus the ones a
//!   container query owns and the ones whose `RuntimeEnabledFeatures` flag is not stable in 150 (`InvertedColors`,
//!   `CSSVideoDynamicRangeMediaQueries`, `PrefersReducedData`, `MediaQueryNavigationControls`,
//!   `DesktopPWAsAdditionalWindowingControls`, `CSSCustomMedia`), each value's type checked as Blink checks it;
//! * `media_query_evaluator.cc`: each feature's evaluation, with `CompareValue`'s and `CompareDoubleValue`'s rules
//!   (a negative query value, a 1/64px epsilon), against the desktop browser tab `MediaValues` describes;
//! * `media_query.cc` / `media_query_exp.cc` serialization (`MediaQuery::Serialize`, `MediaQueryExp::Serialize`).
//!
//! Every answer here was measured on Chrome 150 (`fixtures/css-answers/media-probe.js`, 290 rows, three runs alike).
//! `(fallback: none)` is not among them: it crashes Chrome 150's renderer, and is unknown here.

use crate::style::calc::{self, Cat, Expr, Unit, UnitKind};
use crate::value::{Length, LengthUnit, ResolveContext};
use cssparser::{ParseError, Parser, ParserInput, Token};

/// What the queries are asked about. Everything here is the persona's: a page that measures the
/// viewport through `@media` and through `window.innerWidth` must get one answer, not two.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MediaContext {
    pub viewport_width: f32,
    pub viewport_height: f32,
    pub device_width: f32,
    pub device_height: f32,
    /// Device pixels per CSS pixel — `devicePixelRatio`, and what `resolution` is asked in.
    pub dppx: f32,
    /// Bits per color component. Zero would mean a monochrome device.
    pub color_bits: u32,
    /// Bits per pixel on a monochrome device; zero on a color one.
    pub monochrome_bits: u32,
    pub prefers_dark: bool,
    pub prefers_reduced_motion: bool,
    /// Whether the primary pointer can hover — false on a touchscreen.
    pub hover: bool,
    /// Whether the primary pointer is coarse (a finger) rather than fine (a mouse).
    pub coarse_pointer: bool,
    /// The media type being rendered for. `print` is what a print stylesheet asks about.
    pub print: bool,
    /// The platform theme's geometry — a classic scrollbar's thickness, a drop-down's theme paddings.
    /// No media feature reads it; it rides here because this is the one value the
    /// layout is already handed from the persona, and the scrollbar a page gives up client width for
    /// is as much the worn device's as its viewport is. The default is no theme at all: no classic
    /// scrollbar and no theme padding, which is what a context with no persona lays out with.
    pub theme: spec::ThemeProfile,
    /// The viewport shows no scrollbars whatever its content: a frame whose `<iframe>` says `scrolling=no`. No media
    /// feature reads it either; the layout of the frame's document does.
    pub scrollbars_off: bool,
}

impl Default for MediaContext {
    /// A 1024x768 desktop screen at 1x — the same starting point `ResolveContext` uses, so a sheet
    /// evaluated before a persona is attached is at least self-consistent.
    fn default() -> Self {
        MediaContext {
            viewport_width: 1024.0,
            viewport_height: 768.0,
            device_width: 1024.0,
            device_height: 768.0,
            dppx: 1.0,
            color_bits: 8,
            monochrome_bits: 0,
            prefers_dark: false,
            prefers_reduced_motion: false,
            hover: true,
            coarse_pointer: false,
            print: false,
            theme: spec::ThemeProfile::default(),
            scrollbars_off: false,
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The tree
// ---------------------------------------------------------------------------------------------------------------

/// `MediaQueryOperator`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CmpOp {
    None,
    Eq,
    Lt,
    Le,
    Gt,
    Ge,
}

impl CmpOp {
    fn text(self) -> &'static str {
        match self {
            CmpOp::None => "",
            CmpOp::Eq => "=",
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
        }
    }

    /// `ReverseOperator`: the left bound reads the other way round.
    fn reversed(self) -> CmpOp {
        match self {
            CmpOp::Lt => CmpOp::Gt,
            CmpOp::Le => CmpOp::Ge,
            CmpOp::Gt => CmpOp::Lt,
            CmpOp::Ge => CmpOp::Le,
            op => op,
        }
    }

    fn lt_le(self) -> bool {
        matches!(self, CmpOp::Lt | CmpOp::Le)
    }

    fn gt_ge(self) -> bool {
        matches!(self, CmpOp::Gt | CmpOp::Ge)
    }
}

/// A feature's value (`MediaQueryExpValue`).
#[derive(Debug, Clone, PartialEq)]
enum Val {
    /// A numeric literal as written (only a math function's own literals are canonicalized).
    Num(f64, Unit),
    /// A math function.
    Math(Expr),
    /// A keyword, as Blink names it.
    Ident(&'static str),
    Ratio(Box<Val>, Box<Val>),
}

impl Val {
    fn css_text(&self) -> String {
        match self {
            Val::Num(v, u) => calc::literal_text(*v, *u),
            Val::Math(e) => e.css_text(),
            Val::Ident(i) => (*i).to_string(),
            Val::Ratio(a, b) => format!("{} / {}", a.css_text(), b.css_text()),
        }
    }

    fn kind(&self) -> Option<UnitKind> {
        match self {
            Val::Num(_, u) => Some(u.kind()),
            Val::Math(e) => match e.category() {
                Cat::Number => Some(UnitKind::Number),
                Cat::Length => Some(UnitKind::Length),
                Cat::Resolution => Some(UnitKind::Resolution),
                _ => None,
            },
            _ => None,
        }
    }

    fn is_number(&self) -> bool {
        self.kind() == Some(UnitKind::Number)
    }

    /// `CSSPrimitiveValue::IsInteger`: an integer literal -- which a whole number from 0 to 255 is, Blink's value
    /// pool -- or any math function of a number.
    fn is_integer(&self) -> bool {
        match self {
            Val::Num(_, u) => *u == Unit::Integer,
            Val::Math(e) => e.category() == Cat::Number,
            _ => false,
        }
    }

    fn is_length(&self) -> bool {
        self.kind() == Some(UnitKind::Length)
    }

    fn is_resolution(&self) -> bool {
        self.kind() == Some(UnitKind::Resolution)
    }

    /// `GetValueIfKnown`.
    fn known(&self) -> Option<f64> {
        match self {
            Val::Num(v, _) => Some(*v),
            Val::Math(e) => e.as_literal().map(|(v, _)| v),
            _ => None,
        }
    }

    /// `Value(media_values)`: the value in its canonical unit -- pixels, dppx, a number.
    fn canonical(&self, ctx: &MediaContext) -> Option<f64> {
        match self {
            Val::Num(v, u) if u.is_relative() => relative_px(*v, *u, ctx),
            Val::Num(v, u) => Some(match u.kind() {
                UnitKind::Number | UnitKind::Percent => *v,
                _ => v * u.factor(),
            }),
            Val::Math(e) => {
                let unknown = std::cell::Cell::new(false);
                let px = |v: f64, u: Unit| {
                    relative_px(v, u, ctx).unwrap_or_else(|| {
                        unknown.set(true);
                        0.0
                    })
                };
                let resolved = e.absolutized(&px)?;
                (!unknown.get()).then(|| resolved.eval(0.0))
            }
            _ => None,
        }
    }
}

/// A relative length in a media query: the initial font for `em`/`rem`/`ex`/`ch` (Blink's `MediaValues`: the
/// default font size, 16px), the viewport for the viewport units.
fn relative_px(value: f64, unit: Unit, ctx: &MediaContext) -> Option<f64> {
    let unit = match unit {
        Unit::Em => LengthUnit::Em,
        Unit::Rem => LengthUnit::Rem,
        Unit::Ex => LengthUnit::Ex,
        Unit::Ch => LengthUnit::Ch,
        Unit::Vw | Unit::Vi | Unit::Svw | Unit::Svi | Unit::Lvw | Unit::Lvi | Unit::Dvw | Unit::Dvi => LengthUnit::Vw,
        Unit::Vh | Unit::Vb | Unit::Svh | Unit::Svb | Unit::Lvh | Unit::Lvb | Unit::Dvh | Unit::Dvb => LengthUnit::Vh,
        Unit::Vmin | Unit::Svmin | Unit::Lvmin | Unit::Dvmin => LengthUnit::Vmin,
        Unit::Vmax | Unit::Svmax | Unit::Lvmax | Unit::Dvmax => LengthUnit::Vmax,
        _ => return None,
    };
    let rc = ResolveContext::new(16.0, 16.0, ctx.viewport_width, ctx.viewport_height);
    Some(Length { value: value as f32, unit }.to_px(&rc) as f64)
}

/// One feature test (`MediaQueryExp`): `(name)`, `(name: value)`, or a range with a bound on either side.
#[derive(Debug, Clone, PartialEq)]
struct Exp {
    name: String,
    left: Option<(Val, CmpOp)>,
    right: Option<(Val, CmpOp)>,
}

impl Exp {
    /// `MediaQueryExpBounds::IsRange`.
    fn is_range(&self) -> bool {
        self.left.is_some() || self.right.as_ref().is_some_and(|(_, op)| *op != CmpOp::None)
    }

    /// `MediaQueryExp::Serialize`.
    fn serialize(&self) -> String {
        if !self.is_range() {
            return match &self.right {
                Some((v, _)) => format!("{}: {}", self.name, v.css_text()),
                None => self.name.clone(),
            };
        }
        let mut out = String::new();
        if let Some((v, op)) = &self.left {
            out.push_str(&format!("{} {} ", v.css_text(), op.text()));
        }
        out.push_str(&self.name);
        if let Some((v, op)) = &self.right {
            out.push_str(&format!(" {} {}", op.text(), v.css_text()));
        }
        out
    }
}

/// A condition (`ConditionalExpNode`).
#[derive(Debug, Clone, PartialEq)]
enum Cond {
    Feature(Exp),
    Not(Box<Cond>),
    And(Box<Cond>, Box<Cond>),
    Or(Box<Cond>, Box<Cond>),
    /// `( … )` around a condition.
    Nested(Box<Cond>),
    /// `<general-enclosed>`, as written: valid, and unknown.
    Unknown(String),
}

impl Cond {
    fn serialize(&self) -> String {
        match self {
            Cond::Feature(e) => e.serialize(),
            Cond::Not(c) => format!("not {}", c.serialize()),
            Cond::And(a, b) => format!("{} and {}", a.serialize(), b.serialize()),
            Cond::Or(a, b) => format!("{} or {}", a.serialize(), b.serialize()),
            Cond::Nested(c) => format!("({})", c.serialize()),
            Cond::Unknown(t) => t.clone(),
        }
    }

    fn eval(&self, ctx: &MediaContext) -> Kleene {
        match self {
            Cond::Feature(e) => Kleene::from(eval_feature(e, ctx)),
            Cond::Not(c) => c.eval(ctx).not(),
            Cond::And(a, b) => match a.eval(ctx) {
                Kleene::False => Kleene::False,
                l => l.and(b.eval(ctx)),
            },
            Cond::Or(a, b) => match a.eval(ctx) {
                Kleene::True => Kleene::True,
                l => l.or(b.eval(ctx)),
            },
            Cond::Nested(c) => c.eval(ctx),
            Cond::Unknown(_) => Kleene::Unknown,
        }
    }
}

/// `KleeneValue`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kleene {
    True,
    False,
    Unknown,
}

impl From<bool> for Kleene {
    fn from(b: bool) -> Self {
        if b {
            Kleene::True
        } else {
            Kleene::False
        }
    }
}

impl Kleene {
    fn not(self) -> Kleene {
        match self {
            Kleene::True => Kleene::False,
            Kleene::False => Kleene::True,
            Kleene::Unknown => Kleene::Unknown,
        }
    }

    fn and(self, o: Kleene) -> Kleene {
        if self == Kleene::False || o == Kleene::False {
            Kleene::False
        } else if self == Kleene::Unknown || o == Kleene::Unknown {
            Kleene::Unknown
        } else {
            Kleene::True
        }
    }

    fn or(self, o: Kleene) -> Kleene {
        if self == Kleene::True || o == Kleene::True {
            Kleene::True
        } else if self == Kleene::Unknown || o == Kleene::Unknown {
            Kleene::Unknown
        } else {
            Kleene::False
        }
    }
}

/// `MediaQuery::RestrictorType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Restrictor {
    None,
    Not,
    Only,
}

#[derive(Debug, Clone, PartialEq)]
struct MediaQuery {
    restrictor: Restrictor,
    /// The media type, lowercased; `all` when the query is a condition alone.
    media_type: String,
    exp: Option<Cond>,
}

impl MediaQuery {
    /// `MediaQuery::CreateNotAll`: what a query that does not parse becomes.
    fn not_all() -> MediaQuery {
        MediaQuery { restrictor: Restrictor::Not, media_type: "all".to_string(), exp: None }
    }

    fn is_not_all(&self) -> bool {
        self.restrictor == Restrictor::Not && self.media_type == "all" && self.exp.is_none()
    }

    /// `MediaQuery::Serialize`.
    fn serialize(&self) -> String {
        let mut out = String::new();
        match self.restrictor {
            Restrictor::Only => out.push_str("only "),
            Restrictor::Not => out.push_str("not "),
            Restrictor::None => {}
        }
        let Some(exp) = &self.exp else {
            out.push_str(&self.media_type);
            return out;
        };
        if self.media_type != "all" || self.restrictor != Restrictor::None {
            out.push_str(&self.media_type);
            out.push_str(" and ");
        }
        out.push_str(&exp.serialize());
        out
    }

    /// `MediaQueryEvaluator::Eval(const MediaQuery&)`, the restrictor applied (`not` leaves unknown unknown).
    fn eval(&self, ctx: &MediaContext) -> Kleene {
        let rendered = if ctx.print { "print" } else { "screen" };
        let type_ok = self.media_type.is_empty() || self.media_type == "all" || self.media_type == rendered;
        let v = if !type_ok {
            Kleene::False
        } else {
            match &self.exp {
                None => Kleene::True,
                Some(c) => c.eval(ctx),
            }
        };
        if self.restrictor == Restrictor::Not {
            v.not()
        } else {
            v
        }
    }
}

/// A comma-separated list of queries — the list matches if any one of them does.
///
/// `parent` chains an enclosing `@media`, so a nested one is a conjunction without the list having
/// to be flattened into a cross-product.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaQueryList {
    queries: Vec<MediaQuery>,
    parent: Option<u32>,
}

impl MediaQueryList {
    /// Parse a query list (`MediaQueryParser::ParseMediaQuerySet`). A query that does not parse becomes `not all` —
    /// false, but harmless — which is what the spec asks for instead of dropping the whole `@media`.
    pub fn parse(text: &str, parent: Option<u32>) -> MediaQueryList {
        let mut input = ParserInput::new(text);
        let mut p = Parser::new(&mut input);
        MediaQueryList { queries: query_set(&mut p), parent }
    }

    /// Did every query in the list fail to parse? (`not all` written as such serializes the same way.)
    pub fn all_invalid(&self) -> bool {
        !self.queries.is_empty() && self.queries.iter().all(MediaQuery::is_not_all)
    }

    /// The list as `MediaList.mediaText` and `MediaQueryList.media` write it (`MediaQuerySet::MediaText`).
    pub fn serialize(&self) -> String {
        self.queries.iter().map(MediaQuery::serialize).collect::<Vec<_>>().join(", ")
    }

    /// The same list, addressing a pool that starts `offset` entries later — what a sheet needs
    /// when it absorbs another sheet's conditions into its own pool.
    pub fn shifted(mut self, offset: u32) -> MediaQueryList {
        self.parent = self.parent.map(|p| p + offset);
        self
    }

    /// This list alone: an empty list matches, otherwise the first query that is TRUE does.
    fn matches_own(&self, ctx: &MediaContext) -> bool {
        self.queries.is_empty() || self.queries.iter().any(|q| q.eval(ctx) == Kleene::True)
    }

    /// Does this list match? `pool` is where the chained parents live.
    ///
    /// Walked rather than recursed. A sheet cannot produce a cycle — the parser only ever points a
    /// nested `@media` at an *earlier* entry — but the type permits one, and a stack overflow on
    /// malformed input is not a failure mode worth leaving available. A chain longer than the pool
    /// has visited some entry twice, so that is the bound.
    pub fn matches(&self, ctx: &MediaContext, pool: &[MediaQueryList]) -> bool {
        if !self.matches_own(ctx) {
            return false;
        }
        let mut parent = self.parent;
        for _ in 0..=pool.len() {
            let Some(p) = parent else { return true };
            let Some(list) = pool.get(p as usize) else { return false };
            if !list.matches_own(ctx) {
                return false;
            }
            parent = list.parent;
        }
        false
    }
}

/// `text` as a media query list serializes: what `matchMedia(text).media`, `MediaList.mediaText` and an `@media`
/// rule's `conditionText` read back.
pub fn serialize(text: &str) -> String {
    MediaQueryList::parse(text, None).serialize()
}

/// `text`'s queries one by one, each as it serializes: a `MediaList`'s items (`MediaList.item(i)`, `length`). A
/// query that does not parse is an item too, `not all` (Chrome 150: `screen,` is `screen, not all`, length 2).
pub fn serialize_each(text: &str) -> Vec<String> {
    MediaQueryList::parse(text, None).queries.iter().map(MediaQuery::serialize).collect()
}

// ---------------------------------------------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------------------------------------------

type PResult<'i, T> = Result<T, ParseError<'i, ()>>;

fn skip_ws(p: &mut Parser) {
    loop {
        let st = p.state();
        match p.next_including_whitespace() {
            Ok(Token::WhiteSpace(_)) => {}
            _ => {
                p.reset(&st);
                return;
            }
        }
    }
}

fn peek<'i>(p: &mut Parser<'i, '_>) -> Option<Token<'i>> {
    let st = p.state();
    let t = p.next_including_whitespace().ok().cloned();
    p.reset(&st);
    t
}

fn at_end(p: &mut Parser) -> bool {
    peek(p).is_none()
}

fn peek_ident(p: &mut Parser, word: &str) -> bool {
    matches!(peek(p), Some(Token::Ident(ref w)) if w.eq_ignore_ascii_case(word))
}

/// `ConsumeIfIdent`: the keyword and the whitespace after it.
fn consume_ident(p: &mut Parser, word: &str) -> bool {
    if !peek_ident(p, word) {
        return false;
    }
    let _ = p.next_including_whitespace();
    skip_ws(p);
    true
}

/// `ParseImpl` for a query set.
fn query_set(p: &mut Parser) -> Vec<MediaQuery> {
    skip_ws(p);
    if at_end(p) {
        return Vec::new();
    }
    let mut out = Vec::new();
    loop {
        let q = query(p);
        let ok = q.is_some() && (at_end(p) || matches!(peek(p), Some(Token::Comma)));
        out.push(match q {
            Some(q) if ok => q,
            _ => MediaQuery::not_all(),
        });
        if at_end(p) || !until_comma(p) {
            break;
        }
    }
    out
}

/// `ConsumeUntilCommaInclusive`.
fn until_comma(p: &mut Parser) -> bool {
    loop {
        match p.next_including_whitespace() {
            Ok(Token::Comma) => {
                skip_ws(p);
                return true;
            }
            Ok(_) => {}
            Err(_) => return false,
        }
    }
}

/// `ConsumeQuery`: `[not | only]? <media-type> [and <condition-without-or>]?`, or a `<media-condition>`.
fn query(p: &mut Parser) -> Option<MediaQuery> {
    let save = p.state();
    let restrictor = if consume_ident(p, "not") {
        Restrictor::Not
    } else if consume_ident(p, "only") {
        Restrictor::Only
    } else {
        Restrictor::None
    };
    if let Some(Token::Ident(word)) = peek(p) {
        let lower = word.to_ascii_lowercase();
        if !matches!(lower.as_str(), "not" | "and" | "or" | "only" | "layer") {
            let _ = p.next_including_whitespace();
            skip_ws(p);
            if !consume_ident(p, "and") {
                return Some(MediaQuery { restrictor, media_type: lower, exp: None });
            }
            let c = condition(p, false)?;
            return Some(MediaQuery { restrictor, media_type: lower, exp: Some(c) });
        }
    }
    p.reset(&save);
    let c = condition(p, true)?;
    Some(MediaQuery { restrictor: Restrictor::None, media_type: "all".to_string(), exp: Some(c) })
}

/// `ConditionalParser::ConsumeCondition`.
fn condition(p: &mut Parser, allow_or: bool) -> Option<Cond> {
    if consume_ident(p, "not") {
        return inner(p).map(|c| Cond::Not(Box::new(c)));
    }
    let mut result = inner(p);
    if peek_ident(p, "and") {
        while result.is_some() && consume_ident(p, "and") {
            result = match (result, inner(p)) {
                (Some(a), Some(b)) => Some(Cond::And(Box::new(a), Box::new(b))),
                _ => None,
            };
        }
    } else if result.is_some() && peek_ident(p, "or") && allow_or {
        while result.is_some() && consume_ident(p, "or") {
            result = match (result, inner(p)) {
                (Some(a), Some(b)) => Some(Cond::Or(Box::new(a), Box::new(b))),
                _ => None,
            };
        }
    }
    result
}

/// `ConditionalParser::ConsumeInner`: `( <feature> )`, `( <condition> )`, or `<general-enclosed>`.
fn inner(p: &mut Parser) -> Option<Cond> {
    if matches!(peek(p), Some(Token::ParenthesisBlock)) {
        let save = p.state();
        let _ = p.next_including_whitespace();
        let parsed = p.parse_nested_block(|p| -> PResult<'_, Cond> {
            skip_ws(p);
            let st = p.state();
            let node = match leaf(p) {
                Some(n) => Some(n),
                None => {
                    p.reset(&st);
                    condition(p, true)
                }
            };
            skip_ws(p);
            match node {
                Some(n) if at_end(p) => Ok(n),
                _ => Err(p.new_custom_error(())),
            }
        });
        match parsed {
            Ok(n) => {
                skip_ws(p);
                return Some(Cond::Nested(Box::new(n)));
            }
            Err(_) => p.reset(&save),
        }
    }
    general_enclosed(p)
}

/// `ConditionalParser::ConsumeGeneralEnclosed`: a parenthesised block or a function, any value inside, kept as the
/// text it was written as.
fn general_enclosed(p: &mut Parser) -> Option<Cond> {
    // the comments before it are not part of it (Blink's tokenizer drops them)
    loop {
        let st = p.state();
        match p.next_including_whitespace_and_comments() {
            Ok(Token::Comment(_)) => {}
            _ => {
                p.reset(&st);
                break;
            }
        }
    }
    let st = p.state();
    let start = p.position();
    match p.next_including_whitespace() {
        Ok(Token::ParenthesisBlock) | Ok(Token::Function(_)) => {}
        _ => {
            p.reset(&st);
            return None;
        }
    }
    let _ = p.parse_nested_block(|p| -> PResult<'_, ()> {
        while p.next_including_whitespace_and_comments().is_ok() {}
        Ok(())
    });
    let text = p.slice_from(start).to_string();
    skip_ws(p);
    Some(Cond::Unknown(text))
}

/// `ConsumeLeaf`.
fn leaf(p: &mut Parser) -> Option<Cond> {
    skip_ws(p);
    let f = feature(p)?;
    skip_ws(p);
    Some(Cond::Feature(f))
}

/// `ConsumeComparison`.
fn comparison(p: &mut Parser) -> Option<CmpOp> {
    let c = match peek(p) {
        Some(Token::Delim(c @ ('=' | '<' | '>'))) => c,
        _ => return None,
    };
    let _ = p.next_including_whitespace();
    if c == '=' {
        skip_ws(p);
        return Some(CmpOp::Eq);
    }
    if matches!(peek(p), Some(Token::Delim('='))) {
        let _ = p.next_including_whitespace();
        skip_ws(p);
        return Some(if c == '<' { CmpOp::Le } else { CmpOp::Ge });
    }
    skip_ws(p);
    Some(if c == '<' { CmpOp::Lt } else { CmpOp::Gt })
}

/// `ConsumeAllowedName`: a feature name a media query may ask about, lowercased, and the whitespace after it.
fn allowed_name(p: &mut Parser) -> Option<String> {
    let Some(Token::Ident(name)) = peek(p) else { return None };
    let lower = name.to_ascii_lowercase();
    if !is_allowed(&lower) {
        return None;
    }
    let _ = p.next_including_whitespace();
    skip_ws(p);
    Some(lower)
}

/// `ConsumeRangeContextFeatureName` (the name is consumed either way, as Blink's is).
fn range_name(p: &mut Parser) -> Option<String> {
    allowed_name(p).filter(|n| is_range_type(n))
}

/// `SkipUntilComparisonOrColon`.
fn skip_until_comparison_or_colon(p: &mut Parser) {
    loop {
        let st = p.state();
        match p.next_including_whitespace() {
            Ok(Token::Delim('<' | '>' | '=')) | Ok(Token::Colon) => {
                p.reset(&st);
                return;
            }
            Ok(_) => {}
            Err(_) => return,
        }
    }
}

/// `MediaQueryParser::ConsumeFeature`: `<mf-boolean>`, `<mf-plain>`, or one of the `<mf-range>` forms.
fn feature(p: &mut Parser) -> Option<Exp> {
    let start = p.state();
    if let Some(name) = allowed_name(p) {
        if at_end(p) && allowed_without_value(&name) {
            return Some(Exp { name, left: None, right: None });
        }
        if matches!(peek(p), Some(Token::Colon)) {
            let _ = p.next_including_whitespace();
            skip_ws(p);
            if let Some(v) = value(p, &name) {
                if at_end(p) {
                    return Some(Exp { name, left: None, right: Some((v, CmpOp::None)) });
                }
            }
        }
    }
    p.reset(&start);
    // `<mf-name> <mf-comparison> <mf-value>`
    if let Some(name) = range_name(p) {
        if !at_end(p) {
            if let Some(op) = comparison(p) {
                if let Some(v) = value(p, &name) {
                    if at_end(p) {
                        return Some(Exp { name, left: None, right: Some((v, op)) });
                    }
                }
            }
        }
    }
    p.reset(&start);
    // `<mf-value> <mf-comparison> <mf-name> [<mf-comparison> <mf-value>]`: the name first, then back to the value
    skip_until_comparison_or_colon(p);
    if at_end(p) {
        return None;
    }
    let after_value1 = p.position();
    let op1 = comparison(p)?;
    let name = range_name(p)?;
    skip_ws(p);
    let after_name = p.state();
    p.reset(&start);
    let v1 = value(p, &name)?;
    if p.position() != after_value1 {
        return None; // something between the first value and its comparison
    }
    p.reset(&after_name);
    if at_end(p) {
        return Some(Exp { name, left: Some((v1, op1)), right: None });
    }
    let op2 = comparison(p)?;
    if !((op1.lt_le() && op2.lt_le()) || (op1.gt_ge() && op2.gt_ge())) {
        return None;
    }
    let v2 = value(p, &name)?;
    Some(Exp { name, left: Some((v1, op1)), right: Some((v2, op2)) })
}

/// A numeric token's value and unit, the whitespace after it consumed. `integer_only`: only a number written as an
/// integer (`ConsumeInteger`).
fn number_token(p: &mut Parser, integer_only: bool, non_negative: bool) -> Option<Val> {
    let st = p.state();
    let start = p.position();
    if !matches!(p.next_including_whitespace(), Ok(Token::Number { .. })) {
        p.reset(&st);
        return None;
    }
    let parsed = calc::number_of(p.slice_from(start));
    let Some((v, integer)) = parsed.filter(|&(v, integer)| !(integer_only && !integer) && !(non_negative && v < 0.0)) else {
        p.reset(&st);
        return None;
    };
    skip_ws(p);
    Some(Val::Num(v, calc::pooled_unit(v, if integer { Unit::Integer } else { Unit::Number })))
}

/// A math function of one of `cats`, or nothing consumed.
fn math(p: &mut Parser, cats: &[Cat], percent: bool) -> Option<Val> {
    if !matches!(peek(p), Some(Token::Function(_))) {
        return None;
    }
    let st = p.state();
    match calc::parse_function_with(p, percent) {
        Some(e) if cats.contains(&e.category()) => {
            skip_ws(p);
            Some(Val::Math(e))
        }
        _ => {
            p.reset(&st);
            None
        }
    }
}

/// A dimension token of `kind`, non-negative when asked.
fn dimension(p: &mut Parser, kind: UnitKind, non_negative: bool) -> Option<Val> {
    let st = p.state();
    let start = p.position();
    let unit = match p.next_including_whitespace() {
        Ok(Token::Dimension { unit, .. }) => Unit::dimension(unit),
        _ => None,
    };
    let Some(unit) = unit.filter(|u| u.kind() == kind) else {
        p.reset(&st);
        return None;
    };
    let Some((v, _)) = calc::number_of(p.slice_from(start)).filter(|&(v, _)| !(non_negative && v < 0.0)) else {
        p.reset(&st);
        return None;
    };
    skip_ws(p);
    Some(Val::Num(v, unit))
}

/// `MediaQueryExpValue::Consume`: an integer, a number, a length, a resolution or a keyword, in that order, then the
/// ratio forms and the type the feature takes.
fn value(p: &mut Parser, feature: &str) -> Option<Val> {
    // ConsumeInteger
    let mut v = number_token(p, true, false).or_else(|| math(p, &[Cat::Number], true));
    // ConsumeNumber, unless the feature wants an integer
    if v.is_none() && !expects_integer(feature) {
        v = number_token(p, false, false).or_else(|| math(p, &[Cat::Number], true));
    }
    // ConsumeLength: a bare zero is a length too (`ShouldAcceptUnitlessLength`), which a number above already took
    if v.is_none() {
        v = dimension(p, UnitKind::Length, false).or_else(|| math(p, &[Cat::Length], true));
    }
    // ConsumeResolution: never negative, and a math function with no percentage
    if v.is_none() {
        v = dimension(p, UnitKind::Resolution, true).or_else(|| math(p, &[Cat::Resolution], false));
    }
    let Some(v) = v else {
        let Some(Token::Ident(word)) = peek(p) else { return None };
        let id = valid_ident(feature, &word.to_ascii_lowercase())?;
        let _ = p.next_including_whitespace();
        skip_ws(p);
        return Some(Val::Ident(id));
    };
    if is_aspect_ratio(feature) {
        if v.known().is_some_and(|n| n < 0.0) {
            return None;
        }
        if !matches!(peek(p), Some(Token::Delim('/'))) {
            return Some(Val::Ratio(Box::new(v), Box::new(Val::Num(1.0, Unit::Integer))));
        }
        let _ = p.next_including_whitespace();
        skip_ws(p);
        let d = number_token(p, false, true).or_else(|| math(p, &[Cat::Number], true))?;
        if v.known() == Some(0.0) && d.known() == Some(0.0) {
            return Some(Val::Ratio(Box::new(Val::Num(1.0, Unit::Integer)), Box::new(Val::Num(0.0, Unit::Integer))));
        }
        return Some(Val::Ratio(Box::new(v), Box::new(d)));
    }
    let ok = (v.is_integer() && expects_integer(feature))
        || (v.is_number() && takes_number(feature))
        || (v.is_integer() && feature == "grid" && v.known().is_none_or(|n| n == 0.0 || n == 1.0))
        || ((v.is_length() || (v.is_number() && v.known() == Some(0.0))) && takes_length(feature))
        || (v.is_resolution() && !v.known().is_some_and(|n| n < 0.0) && takes_resolution(feature));
    ok.then_some(v)
}

// ---------------------------------------------------------------------------------------------------------------
// The feature tables (`media_feature_names.json5`, `media_query_parser.cc`, `media_query_exp.cc`)
// ---------------------------------------------------------------------------------------------------------------

/// `MediaQueryFeatureSet::IsAllowed`: every name but a container query's own and a custom media name.
fn is_allowed(name: &str) -> bool {
    !matches!(
        name,
        "inline-size" | "min-inline-size" | "max-inline-size" | "block-size" | "min-block-size" | "max-block-size"
            | "stuck" | "snapped" | "scrollable" | "scrolled"
    ) && !name.starts_with("--")
}

/// `IsAllowedWithoutValue`, the flags of Chrome 150 stable.
fn allowed_without_value(name: &str) -> bool {
    matches!(
        name,
        "monochrome" | "color" | "color-index" | "grid" | "height" | "width" | "device-height" | "device-width"
            | "orientation" | "aspect-ratio" | "device-aspect-ratio" | "hover" | "any-hover" | "-webkit-transform-3d"
            | "pointer" | "any-pointer" | "-webkit-device-pixel-ratio" | "resolution" | "display-mode" | "scan"
            | "color-gamut" | "prefers-color-scheme" | "prefers-contrast" | "prefers-reduced-motion" | "overflow-inline"
            | "overflow-block" | "update" | "prefers-reduced-transparency" | "forced-colors"
            | "horizontal-viewport-segments" | "vertical-viewport-segments" | "device-posture" | "scripting"
    )
}

/// `IsRangeTypeFeature`.
fn is_range_type(name: &str) -> bool {
    matches!(
        name,
        "height" | "width" | "device-height" | "device-width" | "aspect-ratio" | "device-aspect-ratio" | "resolution"
            | "color" | "color-index" | "monochrome" | "-webkit-device-pixel-ratio" | "horizontal-viewport-segments"
            | "vertical-viewport-segments"
    )
}

/// `FeatureExpectingInteger` (viewport segments: `ViewportSegments`, stable).
fn expects_integer(name: &str) -> bool {
    matches!(
        name,
        "color" | "max-color" | "min-color" | "color-index" | "max-color-index" | "min-color-index" | "monochrome"
            | "max-monochrome" | "min-monochrome" | "horizontal-viewport-segments" | "vertical-viewport-segments"
    )
}

/// `FeatureWithNumber`.
fn takes_number(name: &str) -> bool {
    matches!(name, "-webkit-transform-3d" | "-webkit-device-pixel-ratio" | "-webkit-max-device-pixel-ratio" | "-webkit-min-device-pixel-ratio")
}

/// `FeatureWithValidLength` (the container sizes are not allowed in a media query at all).
fn takes_length(name: &str) -> bool {
    matches!(
        name,
        "height" | "max-height" | "min-height" | "width" | "max-width" | "min-width" | "device-height"
            | "max-device-height" | "min-device-height" | "device-width" | "min-device-width" | "max-device-width"
    )
}

/// `FeatureWithValidDensity`.
fn takes_resolution(name: &str) -> bool {
    matches!(name, "resolution" | "min-resolution" | "max-resolution")
}

/// `FeatureWithAspectRatio`.
fn is_aspect_ratio(name: &str) -> bool {
    matches!(
        name,
        "aspect-ratio" | "device-aspect-ratio" | "min-aspect-ratio" | "max-aspect-ratio" | "min-device-aspect-ratio"
            | "max-device-aspect-ratio"
    )
}

/// `FeatureWithValidIdent`: the keyword, as Blink names it, when `feature` takes it in Chrome 150. Features behind a
/// flag that is not stable there take none (`inverted-colors`, `video-dynamic-range`, `prefers-reduced-data`,
/// `navigation-controls`, `display-state`, `resizable`); `fallback` is left out (its query crashes the renderer).
fn valid_ident(feature: &str, word: &str) -> Option<&'static str> {
    let allowed: &[&'static str] = match feature {
        "display-mode" => &["fullscreen", "unframed", "standalone", "minimal-ui", "window-controls-overlay", "browser", "tabbed", "picture-in-picture"],
        "orientation" => &["portrait", "landscape"],
        "pointer" | "any-pointer" => &["none", "coarse", "fine"],
        "hover" | "any-hover" => &["none", "hover"],
        "scan" => &["interlace", "progressive"],
        "color-gamut" => &["srgb", "p3", "rec2020"],
        "prefers-color-scheme" => &["dark", "light"],
        "prefers-contrast" => &["no-preference", "more", "less", "custom"],
        "prefers-reduced-motion" | "prefers-reduced-transparency" => &["no-preference", "reduce"],
        "dynamic-range" => &["standard", "high"],
        "forced-colors" => &["none", "active"],
        "device-posture" => &["continuous", "folded"],
        "overflow-inline" => &["none", "scroll"],
        "overflow-block" => &["none", "scroll", "paged"],
        "update" => &["none", "fast", "slow"],
        "scripting" => &["enabled", "initial-only", "none"],
        _ => &[],
    };
    allowed.iter().copied().find(|a| *a == word)
}

// ---------------------------------------------------------------------------------------------------------------
// Evaluation (`media_query_evaluator.cc`, against a desktop browser tab's `MediaValues`)
// ---------------------------------------------------------------------------------------------------------------

/// `MediaQueryEvaluator::EvalFeature`: the right bound as written, the left one reversed.
fn eval_feature(e: &Exp, ctx: &MediaContext) -> bool {
    let mut result = true;
    if !e.is_range() || e.right.is_some() {
        let (v, op) = match &e.right {
            Some((v, op)) => (Some(v), *op),
            None => (None, CmpOp::None),
        };
        result &= eval_one(&e.name, v, op, ctx);
    }
    if let Some((v, op)) = &e.left {
        result &= eval_one(&e.name, Some(v), op.reversed(), ctx);
    }
    result
}

/// `HandleNegativeMediaFeatureValue`: a negative query value is false, unless it is a lower bound.
fn negative(op: CmpOp) -> bool {
    matches!(op, CmpOp::Gt | CmpOp::Ge)
}

/// `CompareValue`.
fn compare<T: PartialOrd + Default + Copy>(actual: T, query: T, op: CmpOp) -> bool {
    if query < T::default() {
        return negative(op);
    }
    match op {
        CmpOp::Ge => actual >= query,
        CmpOp::Le => actual <= query,
        CmpOp::Eq | CmpOp::None => actual == query,
        CmpOp::Lt => actual < query,
        CmpOp::Gt => actual > query,
    }
}

/// `CompareDoubleValue`: within a LayoutUnit's epsilon.
fn compare_double(actual: f64, query: f64, op: CmpOp) -> bool {
    if query < 0.0 {
        return negative(op);
    }
    const EPS: f64 = 1.0 / 64.0;
    match op {
        CmpOp::Ge => actual >= query - EPS,
        CmpOp::Le => actual <= query + EPS,
        CmpOp::Eq | CmpOp::None => (actual - query).abs() <= EPS,
        CmpOp::Lt => actual < query,
        CmpOp::Gt => actual > query,
    }
}

/// `ClampTo<int>` of a float.
fn to_int(v: f32) -> i32 {
    if v.is_nan() {
        0
    } else {
        v.clamp(i32::MIN as f32, i32::MAX as f32) as i32
    }
}

/// `NumberValue`: a number's value as a float.
fn number(v: &Val, ctx: &MediaContext) -> Option<f32> {
    v.is_number().then(|| v.canonical(ctx)).flatten().map(|n| n as f32)
}

/// An integer feature compared with `actual`.
fn int_feature(v: Option<&Val>, op: CmpOp, ctx: &MediaContext, actual: i32, boolean: bool) -> bool {
    match v {
        Some(v) => number(v, ctx).is_some_and(|n| compare(actual, to_int(n), op)),
        None => boolean,
    }
}

/// `ComputeLengthAndCompare`: a length (or a bare zero) against `actual` pixels.
fn length_feature(v: Option<&Val>, op: CmpOp, ctx: &MediaContext, actual: f64) -> bool {
    let Some(v) = v else { return actual != 0.0 };
    let length = if v.is_number() {
        // `ComputeLength`: a number is a length only when it is zero, in a standards-mode document
        match v.canonical(ctx) {
            Some(n) if to_int(n as f32) == 0 => 0.0,
            _ => return false,
        }
    } else {
        match v.canonical(ctx) {
            Some(n) => n,
            None => return false,
        }
    };
    compare_double(actual, length, op)
}

/// `CompareAspectRatioValue`.
fn aspect_feature(v: Option<&Val>, op: CmpOp, ctx: &MediaContext, width: f32, height: f32) -> bool {
    let Some(v) = v else { return true };
    let Val::Ratio(a, b) = v else { return false };
    let (Some(n), Some(d)) = (a.canonical(ctx), b.canonical(ctx)) else { return false };
    compare_double((width as i32) as f64 * d, (height as i32) as f64 * n, op)
}

/// `EvalResolution`.
fn resolution(v: Option<&Val>, op: CmpOp, ctx: &MediaContext) -> bool {
    let actual: f32 = if ctx.print { 300.0 / 96.0 } else { ctx.dppx };
    let Some(v) = v else { return actual != 0.0 };
    if v.is_number() {
        return v.canonical(ctx).is_some_and(|n| compare(actual, n as f32, op));
    }
    if !v.is_resolution() {
        return false;
    }
    let Some(dppx) = v.canonical(ctx).map(|n| n as f32) else { return false };
    if matches!(v, Val::Num(_, Unit::Dpcm)) {
        let round = |x: f32| (0.5 + 100.0 * x).floor() / 100.0;
        return compare(round(actual), round(dppx), op);
    }
    compare(actual, dppx, op)
}

fn ident(v: Option<&Val>) -> Option<&'static str> {
    match v {
        Some(Val::Ident(i)) => Some(i),
        _ => None,
    }
}

/// One feature's `*MediaFeatureEval`: `value` is `None` in a boolean context.
fn eval_one(name: &str, value: Option<&Val>, op: CmpOp, ctx: &MediaContext) -> bool {
    use CmpOp::{Ge, Le};
    let can_scroll = !ctx.print;
    // the keyword features: a boolean context answers whether the feature is not its `none`
    let keyword = |boolean: bool, matches: &dyn Fn(&str) -> bool| match value {
        None => boolean,
        Some(v) => ident(Some(v)).is_some_and(matches),
    };
    match name {
        "width" => length_feature(value, op, ctx, ctx.viewport_width as f64),
        "min-width" => length_feature(value, Ge, ctx, ctx.viewport_width as f64),
        "max-width" => length_feature(value, Le, ctx, ctx.viewport_width as f64),
        "height" => length_feature(value, op, ctx, ctx.viewport_height as f64),
        "min-height" => length_feature(value, Ge, ctx, ctx.viewport_height as f64),
        "max-height" => length_feature(value, Le, ctx, ctx.viewport_height as f64),
        // a device is never zero-sized: `(device-width)` is true
        "device-width" => value.is_none() || length_feature(value, op, ctx, ctx.device_width as f64),
        "min-device-width" => value.is_none() || length_feature(value, Ge, ctx, ctx.device_width as f64),
        "max-device-width" => value.is_none() || length_feature(value, Le, ctx, ctx.device_width as f64),
        "device-height" => value.is_none() || length_feature(value, op, ctx, ctx.device_height as f64),
        "min-device-height" => value.is_none() || length_feature(value, Ge, ctx, ctx.device_height as f64),
        "max-device-height" => value.is_none() || length_feature(value, Le, ctx, ctx.device_height as f64),
        "aspect-ratio" => aspect_feature(value, op, ctx, ctx.viewport_width, ctx.viewport_height),
        "min-aspect-ratio" => aspect_feature(value, Ge, ctx, ctx.viewport_width, ctx.viewport_height),
        "max-aspect-ratio" => aspect_feature(value, Le, ctx, ctx.viewport_width, ctx.viewport_height),
        "device-aspect-ratio" => aspect_feature(value, op, ctx, ctx.device_width, ctx.device_height),
        "min-device-aspect-ratio" => aspect_feature(value, Ge, ctx, ctx.device_width, ctx.device_height),
        "max-device-aspect-ratio" => aspect_feature(value, Le, ctx, ctx.device_width, ctx.device_height),
        "color" => int_feature(value, op, ctx, ctx.color_bits as i32, ctx.color_bits != 0),
        "min-color" => int_feature(value, Ge, ctx, ctx.color_bits as i32, ctx.color_bits != 0),
        "max-color" => int_feature(value, Le, ctx, ctx.color_bits as i32, ctx.color_bits != 0),
        // no colour lookup table: `(color-index)` is false, its value 0
        "color-index" => int_feature(value, op, ctx, 0, false),
        "min-color-index" => int_feature(value, Ge, ctx, 0, false),
        "max-color-index" => int_feature(value, Le, ctx, 0, false),
        "monochrome" => int_feature(value, op, ctx, ctx.monochrome_bits as i32, ctx.monochrome_bits != 0),
        "min-monochrome" => int_feature(value, Ge, ctx, ctx.monochrome_bits as i32, ctx.monochrome_bits != 0),
        "max-monochrome" => int_feature(value, Le, ctx, ctx.monochrome_bits as i32, ctx.monochrome_bits != 0),
        // a bitmap device: `(grid: 0)` is true, `(grid)` false
        "grid" => value.and_then(|v| number(v, ctx)).is_some_and(|n| compare(to_int(n), 0, op)),
        "resolution" => (value.is_none() || value.is_some_and(Val::is_resolution)) && resolution(value, op, ctx),
        "min-resolution" => (value.is_none() || value.is_some_and(Val::is_resolution)) && resolution(value, Ge, ctx),
        "max-resolution" => (value.is_none() || value.is_some_and(Val::is_resolution)) && resolution(value, Le, ctx),
        "-webkit-device-pixel-ratio" => (value.is_none() || value.is_some_and(Val::is_number)) && resolution(value, op, ctx),
        "-webkit-min-device-pixel-ratio" => (value.is_none() || value.is_some_and(Val::is_number)) && resolution(value, Ge, ctx),
        "-webkit-max-device-pixel-ratio" => (value.is_none() || value.is_some_and(Val::is_number)) && resolution(value, Le, ctx),
        "orientation" => match ident(value) {
            Some(o) => o == if ctx.viewport_width > ctx.viewport_height { "landscape" } else { "portrait" },
            None => ctx.viewport_width >= 0.0 && ctx.viewport_height >= 0.0,
        },
        // a compositing browser renders 3D: `(-webkit-transform-3d)` is true, its value 1
        "-webkit-transform-3d" => int_feature(value, op, ctx, 1, true),
        "hover" | "any-hover" => keyword(ctx.hover, &|i| i == if ctx.hover { "hover" } else { "none" }),
        "pointer" | "any-pointer" => {
            let primary = if ctx.coarse_pointer { "coarse" } else { "fine" };
            keyword(true, &|i| i == primary)
        }
        "display-mode" => keyword(true, &|i| i == "browser"),
        // `scan` applies to `tv` media only
        "scan" => false,
        "color-gamut" => keyword(true, &|i| i == "srgb"),
        "dynamic-range" => value.is_some() && keyword(false, &|i| i == "standard"),
        "prefers-color-scheme" => keyword(true, &|i| i == if ctx.prefers_dark { "dark" } else { "light" }),
        "prefers-contrast" => keyword(false, &|i| i == "no-preference"),
        "prefers-reduced-motion" => {
            keyword(ctx.prefers_reduced_motion, &|i| (i == "no-preference") != ctx.prefers_reduced_motion)
        }
        "prefers-reduced-transparency" => keyword(false, &|i| i == "no-preference"),
        "forced-colors" => keyword(false, &|i| i == "none"),
        "overflow-inline" => keyword(can_scroll, &|i| if i == "none" { !can_scroll } else { can_scroll }),
        "overflow-block" => keyword(true, &|i| match i {
            "scroll" => can_scroll,
            "paged" => !can_scroll,
            _ => false,
        }),
        "update" => keyword(can_scroll, &|i| match i {
            "none" => !can_scroll,
            "fast" => can_scroll,
            _ => false,
        }),
        // one unfolded screen: `(device-posture: continuous)`, one segment each way
        "device-posture" => keyword(true, &|i| i == "continuous"),
        "horizontal-viewport-segments" | "vertical-viewport-segments" => int_feature(value, op, ctx, 1, true),
        "scripting" => keyword(true, &|i| i == "enabled"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> MediaContext {
        MediaContext::default()
    }

    fn m(query: &str) -> bool {
        matches_in(query, &ctx())
    }

    fn matches_in(query: &str, c: &MediaContext) -> bool {
        let list = MediaQueryList::parse(query, None);
        list.matches(c, &[])
    }

    #[test]
    fn a_media_type_is_matched_against_what_is_being_rendered_for() {
        assert!(m("screen"));
        assert!(m("all"));
        assert!(!m("print"));
        assert!(!m("speech"), "a type nothing renders for never matches");
        let print = MediaContext { print: true, ..ctx() };
        assert!(matches_in("print", &print));
        assert!(!matches_in("screen", &print));
    }

    #[test]
    fn only_is_a_no_op_and_not_inverts() {
        assert!(m("only screen"));
        assert!(!m("not screen"));
        assert!(m("not print"));
    }

    #[test]
    fn min_and_max_are_comparisons_written_into_the_name() {
        assert!(m("(min-width: 600px)"), "the default viewport is 1024px wide");
        assert!(!m("(min-width: 1200px)"));
        assert!(m("(max-width: 1200px)"));
        assert!(!m("(max-width: 600px)"));
        assert!(m("(width: 1024px)"), "the plain form is equality");
        assert!(!m("(width: 1000px)"));
    }

    // MQ4's range syntax says the same things, and both directions have to read the same.
    #[test]
    fn the_range_syntax_says_what_min_and_max_say() {
        assert!(m("(width >= 600px)"));
        assert!(!m("(width >= 1200px)"));
        assert!(m("(600px <= width)"), "the feature may come second");
        assert!(!m("(1200px <= width)"));
        assert!(m("(width < 2000px)"));
        assert!(m("(400px <= width <= 2000px)"));
        assert!(!m("(400px <= width <= 700px)"));
    }

    #[test]
    fn and_joins_conditions_and_a_comma_separates_alternatives() {
        assert!(m("screen and (min-width: 600px)"));
        assert!(!m("screen and (min-width: 600px) and (min-height: 2000px)"));
        assert!(m("print, screen"), "either alternative is enough");
        assert!(!m("print, speech"));
    }

    #[test]
    fn units_are_resolved_before_they_are_compared() {
        assert!(m("(min-width: 40em)"), "40em is 640px against the initial font size");
        assert!(!m("(min-width: 80em)"));
        assert!(m("(min-width: 10in)"), "960px");
    }

    #[test]
    fn orientation_comes_from_the_viewport_not_the_device() {
        assert!(m("(orientation: landscape)"));
        let tall = MediaContext { viewport_width: 400.0, viewport_height: 800.0, ..ctx() };
        assert!(matches_in("(orientation: portrait)", &tall));
        assert!(!matches_in("(orientation: landscape)", &tall));
    }

    #[test]
    fn the_preference_features_read_the_persona() {
        assert!(m("(prefers-color-scheme: light)"));
        assert!(!m("(prefers-color-scheme: dark)"));
        let dark = MediaContext { prefers_dark: true, ..ctx() };
        assert!(matches_in("(prefers-color-scheme: dark)", &dark));
        let still = MediaContext { prefers_reduced_motion: true, ..ctx() };
        assert!(matches_in("(prefers-reduced-motion: reduce)", &still));
        assert!(m("(prefers-reduced-motion: no-preference)"));
    }

    #[test]
    fn a_touchscreen_persona_answers_the_pointer_features_differently() {
        let phone = MediaContext { hover: false, coarse_pointer: true, ..ctx() };
        assert!(matches_in("(hover: none)", &phone));
        assert!(matches_in("(pointer: coarse)", &phone));
        assert!(!matches_in("(hover: hover)", &phone));
        assert!(m("(hover: hover)"));
        assert!(m("(pointer: fine)"));
    }

    #[test]
    fn resolution_is_compared_in_dppx_whatever_it_is_written_in() {
        let retina = MediaContext { dppx: 2.0, ..ctx() };
        assert!(matches_in("(min-resolution: 2dppx)", &retina));
        assert!(matches_in("(min-resolution: 192dpi)", &retina));
        assert!(matches_in("(min-resolution: 2x)", &retina));
        assert!(!matches_in("(min-resolution: 3dppx)", &retina));
        assert!(!m("(min-resolution: 2dppx)"));
    }

    #[test]
    fn aspect_ratio_takes_a_fraction() {
        assert!(m("(min-aspect-ratio: 1/1)"), "1024x768 is 4:3");
        assert!(!m("(min-aspect-ratio: 2/1)"));
        assert!(m("(max-aspect-ratio: 16/9)"));
    }

    // A boolean feature is true when the device has any of it -- the usual way pages ask "is this a
    // color screen at all".
    #[test]
    fn the_boolean_form_asks_whether_the_device_has_the_feature() {
        assert!(m("(color)"));
        assert!(!m("(monochrome)"));
        assert!(m("(hover)"));
        let phone = MediaContext { hover: false, coarse_pointer: true, ..ctx() };
        assert!(!matches_in("(hover)", &phone));
    }

    // The point: a query naming something this engine has never heard of is false, and takes
    // nothing else down with it.
    #[test]
    fn an_unknown_feature_is_false_and_not_an_error() {
        assert!(!m("(min-inverted-colors: 1)"));
        assert!(!m("screen and (min-inverted-colors: 1)"));
        assert!(m("screen, (min-inverted-colors: 1)"), "the other alternative still matches");
    }

    #[test]
    fn a_malformed_query_is_false_rather_than_fatal() {
        assert!(!m("(min-width"));
        assert!(!m("and screen"));
        assert!(!m("(("));
        assert!(m("print, screen and (min-width: 1px)"));
    }

    // A nested `@media` is a conjunction, and chaining it means the list never has to be flattened
    // into a cross-product of its alternatives.
    #[test]
    fn a_nested_query_only_matches_when_its_parent_does() {
        let pool = vec![
            MediaQueryList::parse("screen", None),
            MediaQueryList::parse("(min-width: 600px)", Some(0)),
            MediaQueryList::parse("(min-width: 600px)", Some(2)),
        ];
        assert!(pool[1].matches(&ctx(), &pool));
        let print = MediaContext { print: true, ..ctx() };
        assert!(!pool[1].matches(&print, &pool), "the parent no longer matches");
        // A cycle cannot come out of the parser, but it must not be able to hang the process
        // either: this used to recurse until the stack ran out.
        assert!(!pool[2].matches(&ctx(), &pool), "a parent that points at itself is not a match");
    }

    /// The rows BrowserScan and a fingerprinting library read, and the serializations around them, as Chrome 150 answers
    /// (`fixtures/css-answers/media-chrome-150.json`).
    #[test]
    fn chrome_150s_features_and_serialization() {
        for (q, matches, media) in [
            ("(inverted-colors: none)", false, "(inverted-colors: none)"),
            ("(video-dynamic-range: standard)", false, "(video-dynamic-range: standard)"),
            ("(dynamic-range)", false, "(dynamic-range)"),
            ("(device-posture: continuous)", true, "(device-posture: continuous)"),
            ("(horizontal-viewport-segments: 1)", true, "(horizontal-viewport-segments: 1)"),
            ("(-webkit-transform-3d)", true, "(-webkit-transform-3d)"),
            ("(-webkit-transform-3d: 1.0)", true, "(-webkit-transform-3d: 1)"),
            ("(grid: 0)", true, "(grid: 0)"),
            ("(grid: 0.0)", true, "(grid: 0)"),
            ("(scan: progressive)", false, "(scan: progressive)"),
            ("(prefers-color-scheme)", true, "(prefers-color-scheme)"),
            ("(color: 8.5)", false, "(color: 8.5)"),
            ("(min-resolution: -1dppx)", false, "(min-resolution: -1dppx)"),
            ("(min-width: calc(10px + 1em))", true, "(min-width: calc(1em + 10px))"),
            ("(1px < width > 2px)", false, "(1px < width > 2px)"),
            ("(min-width: 1e3px)", true, "(min-width: 1000px)"),
            ("(min-aspect-ratio: 0.01)", true, "(min-aspect-ratio: 0.01 / 1)"),
            ("(aspect-ratio: 0/0)", false, "(aspect-ratio: 1 / 0)"),
            ("(min-aspect-ratio: 1/-2)", false, "(min-aspect-ratio: 1/-2)"),
            ("(  foo  :  bar  )", false, "(  foo  :  bar  )"),
            ("(color) or (foo)", true, "(color) or (foo)"),
            ("not (foo)", false, "not (foo)"),
            ("((color))", true, "((color))"),
            ("screen,", true, "screen, not all"),
            (",screen", true, "not all, screen"),
            ("screen and (color) or (hover)", false, "not all"),
            ("( min-width : 1PX )", true, "(min-width: 1px)"),
            ("(1px<=width)", true, "(1px <= width)"),
            ("not foo(bar)", false, "not foo(bar)"),
            ("(:8)", false, "(:8)"),
            ("(width < 1e9px)", true, "(width < 1e+09px)"),
            ("(color /* c */)", true, "(color)"),
            ("/* c */ screen", true, "screen"),
            ("(color: calc(4 + 4))", true, "(color: calc(8))"),
            ("(min-width: calc(50% + 1px))", false, "(min-width: calc(50% + 1px))"),
            ("(--x)", false, "(--x)"),
            ("Screen And (Color)", true, "screen and (color)"),
            ("not not (color)", false, "not all"),
            ("", true, ""),
        ] {
            let list = MediaQueryList::parse(q, None);
            assert_eq!((list.matches(&ctx(), &[]), list.serialize().as_str()), (matches, media), "{q}");
        }
    }
}
