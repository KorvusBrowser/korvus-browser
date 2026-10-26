//! Stylesheet text in, [`Stylesheet`] out.
//!
//! CSS parsing has no exceptions and almost no failures: a declaration that does not parse is
//! dropped, a rule whose selector does not parse is skipped, and the sheet around them is
//! unaffected. That is a specified behaviour, not leniency — a page that uses a property this
//! engine has never heard of must still get the rest of its stylesheet — so every error path here
//! ends in "skip this and carry on", and the tests say so out loud.
//!
//! # What comes out is a TREE
//!
//! `@media`, `@supports`, `@layer`, `@import` and nested style rules hold rules, and the CSSOM hands
//! the page those rules as children (`CSSMediaRule.cssRules`, `CSSStyleRule.cssRules`). So the parse
//! produces [`Rule`]s with [`Rule::children`], and [`Stylesheet::rebuild`] flattens them into what
//! the cascade matches. Until this unit only `@media` was understood, and it was flattened AT the
//! parse -- so `cssRules` had no `CSSMediaRule`, and `@supports`, `@layer`, `@import` and CSS
//! nesting applied nothing at all. Measured on `/usr/bin/google-chrome` 150.0.7871.46 vs this engine:
//!
//! ```text
//! @layer base { .l { color } }                  chrome rgb(1,2,3)     korvus before rgb(0,0,0)
//! @supports (display:grid) { .s { color } }     chrome rgb(4,5,6)     korvus before rgb(0,0,0)
//! .n { & .c { color } }                         chrome rgb(7,8,9)     korvus before rgb(0,0,0)
//! @import url(imp.css) -> .i { color }          chrome rgb(10,11,12)  korvus before rgb(0,0,0)
//! ```
//!
//! `@container`, `@scope`, `@property`, `@page` and `@starting-style`, which Chrome
//! keeps in `cssRules` as `CSSContainerRule`, `CSSScopeRule`, `CSSPropertyRule`, `CSSPageRule` and
//! `CSSStartingStyleRule`. What is still dropped whole: `@charset`, and `@namespace` -- whose prefixes the
//! sheet's selectors do resolve (`svg|rect`, a default namespace), though `cssRules` holds no
//! `CSSNamespaceRule` for it.

use crate::style::at_rules::{self, DescriptorBlock};
use crate::style::media::MediaQueryList;
use crate::style::property::PropertyId;
use crate::style::rules::{
    anon_layer_id, Declaration, ImportRule, Origin, RawDeclaration, Rule, RuleKind, Span, Stylesheet,
};
use crate::style::shorthand;
use crate::style::variables::{references, References};
use cssparser::{
    AtRuleParser, CowRcStr, Delimiter, ParseError, Parser, ParserInput, ParserState, QualifiedRuleParser,
    RuleBodyItemParser, RuleBodyParser, StyleSheetParser, Token,
};
use korvus_dom::Selector;

/// Parse a stylesheet. Never fails: what cannot be understood is left out.
pub fn parse_stylesheet(origin: Origin, text: &str) -> Stylesheet {
    let mut builder = Builder::new(origin, text, Ctx::TopLevel);
    let rules = builder.rule_list_str(text);
    let mut sheet = builder.sheet;
    sheet.set_rules(rules);
    sheet
}

/// Where [`parse_rule_in`] is parsing: what the CSSOM's `insertRule`/`appendRule` needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertContext {
    /// A sheet's own rule list (`CSSStyleSheet.insertRule`): `@import` is grammatical here.
    Sheet,
    /// A grouping rule outside any style rule (`CSSMediaRule.insertRule`).
    Group,
    /// A style rule, or a grouping rule inside one: relative selectors and declarations.
    Nested,
    /// `CSSKeyframesRule.appendRule`: keyframe blocks.
    Keyframes,
}

/// Parse `text` as rules for the CSSOM to insert in `at`. The sheet it returns holds them as its
/// top-level list, ready to be absorbed; the caller decides whether it is exactly one rule.
pub fn parse_rule_in(origin: Origin, text: &str, at: InsertContext) -> Stylesheet {
    let ctx = match at {
        InsertContext::Sheet => Ctx::TopLevel,
        InsertContext::Group => Ctx::Group,
        InsertContext::Nested => Ctx::Style,
        InsertContext::Keyframes => Ctx::Keyframes,
    };
    let mut builder = Builder::new(origin, text, ctx);
    let rules = if at == InsertContext::Nested {
        builder.nested_rule_list_str(text)
    } else {
        builder.rule_list_str(text)
    };
    let mut sheet = builder.sheet;
    sheet.set_rules(rules);
    sheet
}

/// Parse the contents of a `style="…"` attribute.
///
/// The result is a [`Stylesheet`] with no rules: an inline style is a declaration block with no
/// selector, and modelling it as a rule-less sheet means it shares the value pool, the shorthand
/// expansion, and the `!important` handling instead of growing a second copy of each.
pub fn parse_inline_style(text: &str) -> Stylesheet {
    let mut builder = Builder::new(Origin::Author, text, Ctx::Declarations);
    let mut pi = ParserInput::new(text);
    let mut parser = Parser::new(&mut pi);
    let _ = builder.body(&mut parser, Ctx::Declarations);
    builder.sheet
}

/// Where the parser is, which decides what a qualified rule's prelude means and whether a block may
/// hold declarations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ctx {
    /// The sheet's own rule list: `@import` may still be allowed here.
    TopLevel,
    /// Inside a grouping rule outside any style rule: a rule list, selectors as written.
    Group,
    /// Inside a style rule, or a grouping rule inside one: declarations AND nested rules, and a
    /// selector is relative to the parent (`.c` is `& .c`).
    Style,
    /// Inside `@keyframes`: the preludes are keyframe selectors.
    Keyframes,
    /// A declaration list and nothing else: a keyframe block, a `style` attribute.
    Declarations,
}

/// Accumulates the sheet's pools while cssparser walks the text, and hands the rules back up.
struct Builder {
    sheet: Stylesheet,
    ctx: Ctx,
    /// `@import` is only valid before every other rule but `@charset` and `@layer` statements.
    imports_allowed: bool,
    /// The sheet's `@namespace` rules, (prefix, URL) with `""` the default namespace: its selectors are read under
    /// them. Taken while nothing but `@charset`, `@import`, `@layer` statements and other `@namespace`s came
    /// before (CSS Namespaces §2); a later one is ignored.
    namespaces: Vec<(String, String)>,
}

/// What one item of a block turned out to be. cssparser needs one type for declarations and rules.
enum Item {
    Rule(Rule),
    Decls(Vec<Declaration>, Vec<RawDeclaration>),
}

impl Builder {
    fn new(origin: Origin, text: &str, ctx: Ctx) -> Builder {
        Builder { sheet: Stylesheet::new(origin, text.to_string()), ctx, imports_allowed: ctx == Ctx::TopLevel, namespaces: Vec::new() }
    }

    /// A rule list (a sheet, or the block of a grouping rule outside a style rule).
    fn rule_list_str(&mut self, text: &str) -> Vec<Rule> {
        let mut pi = ParserInput::new(text);
        let mut parser = Parser::new(&mut pi);
        self.rule_list(&mut parser)
    }

    fn rule_list(&mut self, input: &mut Parser) -> Vec<Rule> {
        let mut out = Vec::new();
        let mut sheet_parser = StyleSheetParser::new(input, self);
        while let Some(result) = sheet_parser.next() {
            // An invalid rule is not an error to report, it is a rule that does not exist. cssparser
            // has already skipped to the end of it.
            if let Ok(Item::Rule(rule)) = result {
                out.push(rule);
            }
        }
        out
    }

    /// A text that is one nested rule (for `insertRule` on a style rule).
    fn nested_rule_list_str(&mut self, text: &str) -> Vec<Rule> {
        let mut pi = ParserInput::new(text);
        let mut parser = Parser::new(&mut pi);
        let (decls, custom, children) = self.body(&mut parser, Ctx::Style);
        // A bare declaration list is not a rule `insertRule` accepts -- Chrome turns it into a
        // `CSSNestedDeclarations`, which is what the body already made of it.
        if !decls.is_empty() || !custom.is_empty() {
            let mut rules = vec![Rule { decls, custom, ..Rule::new(RuleKind::NestedDeclarations) }];
            rules.extend(children);
            return rules;
        }
        children
    }

    /// A block's contents under `ctx`: the declarations before the first nested rule (the rule's
    /// OWN declarations), and the nested rules -- with each later run of declarations as a
    /// [`RuleKind::NestedDeclarations`] at its own position, as CSS Nesting 1 §2.3 orders them.
    fn body(&mut self, input: &mut Parser, ctx: Ctx) -> (Span, Span, Vec<Rule>) {
        let outer = std::mem::replace(&mut self.ctx, ctx);
        let outer_imports = std::mem::replace(&mut self.imports_allowed, false);
        let mut items = Vec::new();
        {
            let mut body = RuleBodyParser::new(input, self);
            while let Some(result) = body.next() {
                if let Ok(item) = result {
                    items.push(item);
                }
            }
        }
        self.ctx = outer;
        self.imports_allowed = outer_imports;

        let (mut own_d, mut own_r) = (Vec::new(), Vec::new());
        let (mut run_d, mut run_r) = (Vec::new(), Vec::new());
        let mut children: Vec<Rule> = Vec::new();
        let mut seen_rule = false;
        for item in items {
            match item {
                // a text-kept declaration remembers where it stands among the block's longhands
                Item::Decls(d, r) if !seen_rule => {
                    own_d.extend(d);
                    let after = own_d.len() as u32;
                    own_r.extend(r.into_iter().map(|x| RawDeclaration { after, ..x }));
                }
                Item::Decls(d, r) => {
                    run_d.extend(d);
                    let after = run_d.len() as u32;
                    run_r.extend(r.into_iter().map(|x| RawDeclaration { after, ..x }));
                }
                Item::Rule(rule) => {
                    if !run_d.is_empty() || !run_r.is_empty() {
                        let decls = self.sheet.push_declarations(std::mem::take(&mut run_d));
                        let custom = self.sheet.push_raws(std::mem::take(&mut run_r));
                        children.push(Rule { decls, custom, ..Rule::new(RuleKind::NestedDeclarations) });
                    }
                    seen_rule = true;
                    children.push(rule);
                }
            }
        }
        if !run_d.is_empty() || !run_r.is_empty() {
            let decls = self.sheet.push_declarations(run_d);
            let custom = self.sheet.push_raws(run_r);
            children.push(Rule { decls, custom, ..Rule::new(RuleKind::NestedDeclarations) });
        }
        let decls = self.sheet.push_declarations(own_d);
        let custom = self.sheet.push_raws(own_r);
        (decls, custom, children)
    }

    /// The children of a grouping at-rule: a rule list outside a style rule, a nesting body inside
    /// one (where its own declarations become a leading `CSSNestedDeclarations`).
    fn group_children(&mut self, input: &mut Parser) -> Vec<Rule> {
        match self.ctx {
            Ctx::Style => {
                let (decls, custom, children) = self.body(input, Ctx::Style);
                let mut out = Vec::new();
                if !decls.is_empty() || !custom.is_empty() {
                    out.push(Rule { decls, custom, ..Rule::new(RuleKind::NestedDeclarations) });
                }
                out.extend(children);
                out
            }
            _ => {
                let outer = std::mem::replace(&mut self.ctx, Ctx::Group);
                let outer_imports = std::mem::replace(&mut self.imports_allowed, false);
                let rules = self.rule_list(input);
                self.ctx = outer;
                self.imports_allowed = outer_imports;
                rules
            }
        }
    }
}

/// What a qualified rule's prelude turned out to be. Two grammars share this position: a selector
/// list everywhere, and a keyframe selector inside `@keyframes`.
enum QualifiedPrelude {
    Selectors(Vec<Selector>),
    KeyframeOffsets(Vec<f32>),
}

impl<'i> QualifiedRuleParser<'i> for Builder {
    type Prelude = QualifiedPrelude;
    type QualifiedRule = Item;
    type Error = ();

    fn parse_prelude<'t>(&mut self, input: &mut Parser<'i, 't>) -> Result<QualifiedPrelude, ParseError<'i, ()>> {
        // The prelude parser is already delimited to end before `{`, so draining it yields exactly
        // the selector text. The loop always ends on a failed `next`, which is what flushes a
        // trailing block like `[hidden]` — otherwise the position would stop inside it.
        let start = input.position();
        while input.next().is_ok() {}
        let text = input.slice(start..input.position());
        match self.ctx {
            Ctx::Keyframes => keyframe_offsets(text.trim())
                .map(QualifiedPrelude::KeyframeOffsets)
                .ok_or_else(|| input.new_custom_error(())),
            Ctx::Style => Selector::parse_nested_in(text, &self.namespaces)
                .map(QualifiedPrelude::Selectors)
                .map_err(|_| input.new_custom_error(())),
            Ctx::Declarations => Err(input.new_custom_error(())),
            _ => Selector::parse_list_in(text, &self.namespaces)
                .map(QualifiedPrelude::Selectors)
                .map_err(|_| input.new_custom_error(())),
        }
    }

    fn parse_block<'t>(
        &mut self,
        prelude: QualifiedPrelude,
        _start: &ParserState,
        input: &mut Parser<'i, 't>,
    ) -> Result<Item, ParseError<'i, ()>> {
        match prelude {
            // An empty block is still a rule: `p { }` is a CSSStyleRule the CSSOM can see and edit.
            QualifiedPrelude::Selectors(selectors) => {
                self.imports_allowed = false;
                let (decls, custom, children) = self.body(input, Ctx::Style);
                Ok(Item::Rule(Rule { kind: RuleKind::Style, selectors, decls, custom, children }))
            }
            QualifiedPrelude::KeyframeOffsets(offsets) => {
                // A keyframe block's custom properties are dropped: `var()` inside a keyframe would
                // have to be substituted against the TARGET's variables, which this side of the
                // parse has never seen. Dropping them is the honest half-answer -- the property
                // simply is not in that keyframe -- and it is recorded here rather than silently.
                let (decls, _custom, _) = self.body(input, Ctx::Declarations);
                Ok(Item::Rule(Rule { decls, ..Rule::new(RuleKind::Keyframe { offsets }) }))
            }
        }
    }
}

impl<'i> cssparser::DeclarationParser<'i> for Builder {
    type Declaration = Item;
    type Error = ();

    fn parse_value<'t>(&mut self, name: CowRcStr<'i>, input: &mut Parser<'i, 't>) -> Result<Item, ParseError<'i, ()>> {
        let decl_start = self.sheet.declaration_count();
        let raw_start = self.sheet.raw_count();
        self.declaration(name, input)?;
        Ok(Item::Decls(self.sheet.take_declarations_from(decl_start), self.sheet.take_raw_from(raw_start)))
    }
}

impl Builder {
    /// Push one declaration's longhands (or its text-kept form) onto the end of the pools. The
    /// caller takes them straight back off, so a block's declarations stay contiguous even when
    /// nested rules push their own in between.
    fn declaration<'i>(&mut self, name: CowRcStr<'i>, input: &mut Parser<'i, '_>) -> Result<(), ParseError<'i, ()>> {
        // A custom property is anything starting with `--`. Its name is not folded to lowercase and
        // its value is not interpreted: `--Foo` and `--foo` are different properties, and the value
        // is whatever tokens the author wrote, kept exactly. That is what makes them useful.
        let custom = name.starts_with("--") && name.len() > 2;
        // An ALIAS is its target: `-webkit-transform` IS `transform`, so the later of the two wins
        // and `-webkit-transform` alone transforms (Blink's alias table, `blink_names`). Chrome 150:
        // `transform: scale(2); -webkit-transform: scale(3)` computes `matrix(3, ...)`; this engine
        // dropped every alias, so `-webkit-user-select: none` left `user-select` at `auto`.
        let name: CowRcStr<'i> = if custom { name } else { crate::style::property::canonical_name(&name).into() };
        // An unknown property is not an error the sheet reports; the declaration simply is not
        // there.
        let property = PropertyId::from_name(&name);
        if !custom && property.is_none() && !shorthand::is_shorthand(&name.to_ascii_lowercase()) {
            return Err(input.new_custom_error(()));
        }

        input.skip_whitespace();
        let start = input.position();
        // `!important` is found by delimiting on `!` rather than by scanning text: a `!` inside a
        // function or a string is not a delimiter at nesting level 0, so `content: "!important"`
        // and `width: calc(1px)` are read as the values they are.
        let _ = input.parse_until_before(Delimiter::Bang, |v| -> Result<(), ParseError<'i, ()>> {
            while v.next().is_ok() {}
            Ok(())
        });
        let end = input.position();
        let important = input.try_parse(cssparser::parse_important).is_ok();
        // Anything left over means the `!` was not `!important`. `color: red !importnt` is an
        // invalid declaration, not a red one.
        input.expect_exhausted().map_err(|_| input.new_custom_error(()))?;

        let value = trim_span(input.slice(start..end), start.byte_index());
        // A value containing `var()` (or `env()`) is kept as text alongside the custom properties, whatever
        // property it is for: it cannot be expanded (a shorthand's arity is not yet known) and it
        // cannot be validated (the substituted value might not fit the property at all).
        let deferred = !value.is_empty()
            && match references(input.slice(start..end)) {
                References::None => false,
                References::Some => !custom,
                References::Invalid => return Err(input.new_custom_error(())),
            };
        if custom || deferred {
            // A custom property may legitimately be empty -- `--x: ;` is a valid declaration whose
            // value is the empty token stream -- so it does not take the emptiness check below.
            //
            // The name is interned rather than pointed at: cssparser hands it over as a string, not
            // as a position in the source, and interning it into the same pool the values live in
            // keeps a declaration two spans and nothing else.
            let name = self.sheet.intern(&name);
            self.sheet.push_raw(RawDeclaration { name, value, important, after: 0 });
            return Ok(());
        }
        if value.is_empty() {
            return Err(input.new_custom_error(())); // `color: ;` declares nothing
        }
        // A math function Blink rejects -- one that does not parse or type-check -- makes the declaration invalid
        // (`style::calc`): Chrome 150 drops `width: calc(1px+2px)`, `width: round(10.4px)` and
        // `transition-duration: calc(1s + 1px)`, which this engine applied.
        if !crate::style::calc::math_valid(input.slice(start..end)) {
            return Err(input.new_custom_error(()));
        }
        // `match-parent` is behind Blink's experimental `CSSTextAlignMatchParent` (Chrome 150 stable: off), so
        // `text-align` / `text-align-last: match-parent` is an invalid declaration there -- the UA sheet's `li` rule
        // included: an `<li>` computes its parent's `start`, a `<span style="text-align:match-parent">` in a
        // right-aligned block reads `right`, and its `style.textAlign` is "" (measured). It computed `match-parent`.
        if matches!(property, Some(PropertyId::TextAlign | PropertyId::TextAlignLast)) && input.slice(start..end).trim().eq_ignore_ascii_case("match-parent") {
            return Err(input.new_custom_error(()));
        }
        match property {
            Some(property) if !shorthand::expands(property) => {
                self.sheet.push_declaration(Declaration { property, value, important, implicit: false })
            }
            // A shorthand becomes its longhands here and is never stored: `margin: 0` followed by
            // `margin-top: 4px` has to leave the top at 4px, which no whole-`margin` representation
            // can express. An invalid one expands to nothing, which is Chrome dropping it.
            _ => {
                shorthand::push_expansion(&mut self.sheet, &name.to_ascii_lowercase(), value, important);
            }
        }
        Ok(())
    }
}

/// What an at-rule's prelude turned out to be. Anything not listed here is rejected at the prelude,
/// and cssparser's own recovery walks past its block.
enum AtPrelude {
    /// The media pool index this at-rule's contents sit under, and the list as written.
    Media(u32, String),
    Supports(String, bool),
    /// `@layer` with its names: none (anonymous block), one (block or statement), or several
    /// (statement only).
    Layer(Vec<String>),
    Import(Box<ImportRule>),
    /// `@position-try <dashed-ident>`.
    PositionTry(String),
    /// `@counter-style <counter-style-name>`.
    CounterStyle(String),
    /// `@keyframes <keyframes-name>` (and its `-webkit-` alias, which half the real web still
    /// ships). The name is kept exactly as written: `<custom-ident>` is case-sensitive, so
    /// `@keyframes Slide` is not what `animation-name: slide` asks for.
    Keyframes(String),
    FontFace,
    /// `@container`: the name, and the query text after it.
    Container(Option<String>, String),
    /// `@scope`: the start and end selector lists as written.
    Scope(Option<String>, Option<String>),
    /// `@property <dashed-ident>`.
    Property(String),
    /// `@page <page-selector-list>?`.
    Page(String),
    StartingStyle,
}

/// `@scope [(<scope-start>)]? [to (<scope-end>)]?`: each list must parse as selectors, or the rule is
/// dropped. The lists are kept as the CSSOM serializes them (`start`, `end`).
fn scope_prelude(prelude: &str) -> Option<(Option<String>, Option<String>)> {
    fn nested_list(p: &mut Parser) -> Option<String> {
        let text = p
            .parse_nested_block(|p| -> Result<String, ParseError<'_, ()>> {
                let start = p.position();
                while p.next().is_ok() {}
                Ok(p.slice_from(start).trim().to_string())
            })
            .ok()?;
        let parsed = Selector::parse_list(&text).ok()?;
        Some(parsed.iter().map(Selector::to_css_string).collect::<Vec<_>>().join(", "))
    }
    let mut pi = ParserInput::new(prelude);
    let mut p = Parser::new(&mut pi);
    let start = match p.try_parse(|p| p.expect_parenthesis_block().map(|_| ())) {
        Ok(()) => Some(nested_list(&mut p)?),
        Err(_) => None,
    };
    let end = match p.try_parse(|p| p.expect_ident_matching("to")) {
        Ok(()) => {
            p.expect_parenthesis_block().ok()?;
            Some(nested_list(&mut p)?)
        }
        Err(_) => None,
    };
    p.expect_exhausted().ok()?;
    Some((start, end))
}

/// `@container`'s prelude: `None` when it does not parse. The name is split off, and the query is
/// whitespace-collapsed, as `containerName` and `containerQuery` read them back.
fn container_prelude(prelude: &str) -> Option<(Option<String>, String)> {
    let text = collapse(prelude);
    let cond = crate::style::container::ContainerCondition::parse(&text)?;
    let query = match &cond.name {
        Some(n) => text[n.len()..].trim().to_string(),
        None => text,
    };
    Some((cond.name, query))
}

/// `@property`'s block, with `syntax` written as Chrome serializes a string: double-quoted.
fn property_block(pairs: Vec<(String, String)>) -> DescriptorBlock {
    let mut block = DescriptorBlock::default();
    for (name, value) in pairs {
        let value = if name == "syntax" {
            let mut pi = ParserInput::new(&value);
            let mut p = Parser::new(&mut pi);
            let inner = p.expect_string().map(|s| s.to_string()).ok().filter(|_| p.is_exhausted());
            match inner {
                Some(inner) => {
                    let mut quoted = String::new();
                    let _ = cssparser::serialize_string(&inner, &mut quoted);
                    quoted
                }
                None => value.clone(),
            }
        } else {
            value
        };
        block.set(&name, value);
    }
    block
}

/// `@keyframes <name>`: a `<custom-ident>` or a `<string>`, and nothing else in the prelude.
fn keyframes_rule_name(prelude: &str) -> Option<String> {
    let mut pi = ParserInput::new(prelude);
    let mut p = Parser::new(&mut pi);
    let name = match p.next() {
        Ok(Token::Ident(i)) => i.as_ref().to_string(),
        Ok(Token::QuotedString(s)) => s.as_ref().to_string(),
        _ => return None,
    };
    p.expect_exhausted().ok()?;
    // `none` is not a keyframes name: `animation-name: none` means "no animation", so a rule by
    // that name could never be reached. The CSS-wide keywords are excluded for the same reason.
    if matches!(
        name.to_ascii_lowercase().as_str(),
        "none" | "initial" | "inherit" | "unset" | "revert" | "revert-layer" | "default"
    ) {
        return None;
    }
    Some(name)
}

/// One keyframe selector list — `from`, `to`, `50%`, or a comma-separated mix — as 0..1 offsets.
///
/// `None` drops that block and keeps the rest of the rule, which is what Chrome does with
/// `@keyframes x { 50% {…} bogus {…} }`.
fn keyframe_offsets(prelude: &str) -> Option<Vec<f32>> {
    let mut out = Vec::new();
    for piece in prelude.split(',') {
        let piece = piece.trim();
        let offset = if piece.eq_ignore_ascii_case("from") {
            0.0
        } else if piece.eq_ignore_ascii_case("to") {
            1.0
        } else {
            let pct = piece.strip_suffix('%')?;
            pct.trim().parse::<f32>().ok()? / 100.0
        };
        out.push(offset);
    }
    (!out.is_empty()).then_some(out)
}

/// `@layer` names: `a`, `a.b`, comma-separated. `None` for anything else (`@layer 1`, `@layer a b`).
fn layer_names(prelude: &str) -> Option<Vec<String>> {
    let prelude = prelude.trim();
    if prelude.is_empty() {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    for part in prelude.split(',') {
        let part = part.trim();
        let valid = !part.is_empty()
            && part.split('.').all(|seg| {
                let mut pi = ParserInput::new(seg);
                let mut p = Parser::new(&mut pi);
                matches!(p.next(), Ok(Token::Ident(ref i)) if !matches!(i.to_ascii_lowercase().as_str(), "initial" | "inherit" | "unset" | "revert" | "revert-layer" | "default"))
                    && p.expect_exhausted().is_ok()
                    && seg == seg.trim()
            });
        if !valid {
            return None;
        }
        out.push(part.to_string());
    }
    Some(out)
}

/// `@import <url> [layer | layer(<name>)]? [supports(<condition>)]? <media-query-list>?`
///
/// `None` drops the rule: an unparsable URL, or a `supports()` this engine does not meet -- Chrome
/// leaves such an `@import` out of `cssRules` entirely and never fetches it (measured).
fn import_prelude(sheet: &mut Stylesheet, input: &mut Parser) -> Option<ImportRule> {
    let href = match input.next().ok()?.clone() {
        Token::UnquotedUrl(u) => u.to_string(),
        Token::QuotedString(s) => s.to_string(),
        Token::Function(ref f) if f.eq_ignore_ascii_case("url") => input
            .parse_nested_block(|p| -> Result<String, ParseError<'_, ()>> { Ok(p.expect_string()?.to_string()) })
            .ok()?,
        _ => return None,
    };
    let mut layer = None;
    let mut supports = None;
    input.skip_whitespace();
    let state = input.state();
    match input.next().cloned() {
        Ok(Token::Ident(i)) if i.eq_ignore_ascii_case("layer") => layer = Some(String::new()),
        Ok(Token::Function(f)) if f.eq_ignore_ascii_case("layer") => {
            let name = input
                .parse_nested_block(|p| -> Result<String, ParseError<'_, ()>> {
                    let start = p.position();
                    while p.next().is_ok() {}
                    Ok(p.slice_from(start).trim().to_string())
                })
                .ok()?;
            let names = layer_names(&name)?;
            if names.len() != 1 {
                return None;
            }
            layer = Some(names.into_iter().next().unwrap_or_default());
        }
        _ => input.reset(&state),
    }
    input.skip_whitespace();
    let state = input.state();
    match input.next().cloned() {
        Ok(Token::Function(f)) if f.eq_ignore_ascii_case("supports") => {
            let condition = input
                .parse_nested_block(|p| -> Result<String, ParseError<'_, ()>> {
                    let start = p.position();
                    while p.next().is_ok() {}
                    Ok(p.slice_from(start).trim().to_string())
                })
                .ok()?;
            // `supports(display: grid)` takes a bare declaration as well as a condition.
            let met = crate::style::supports::evaluate(&condition)
                || crate::style::supports::evaluate(&format!("({condition})"));
            if !met {
                return None;
            }
            supports = Some(collapse(&condition));
        }
        _ => input.reset(&state),
    }
    input.skip_whitespace();
    let start = input.position();
    while input.next().is_ok() {}
    let query = input.slice_from(start).trim();
    let media = (!query.is_empty()).then(|| {
        let text = collapse(query);
        (sheet.push_media(MediaQueryList::parse(&text, None)), text)
    });
    Some(ImportRule { href, media, supports, layer, anon: anon_layer_id(), url: None, loaded: false, base: 0 })
}

/// Whitespace collapsing for the preludes the CSSOM reads back.
/// `@namespace [<prefix>]? [<string> | <url>]`, as (prefix, URL) with `""` for no prefix.
fn namespace_prelude(input: &mut Parser) -> Option<(String, String)> {
    let prefix = input.try_parse(|p| p.expect_ident_cloned()).map(|i| i.to_string()).unwrap_or_default();
    let url = match input.next().ok()?.clone() {
        Token::QuotedString(s) | Token::UnquotedUrl(s) => s.to_string(),
        Token::Function(f) if f.eq_ignore_ascii_case("url") => {
            input.parse_nested_block(|p| p.expect_string_cloned().map_err(Into::<ParseError<()>>::into)).ok()?.to_string()
        }
        _ => return None,
    };
    input.expect_exhausted().ok()?;
    Some((prefix, url))
}

fn collapse(text: &str) -> String {
    text.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
}

impl<'i> AtRuleParser<'i> for Builder {
    type Prelude = AtPrelude;
    type AtRule = Item;
    type Error = ();

    fn parse_prelude<'t>(&mut self, name: CowRcStr<'i>, input: &mut Parser<'i, 't>) -> Result<AtPrelude, ParseError<'i, ()>> {
        let lower = name.to_ascii_lowercase();
        if self.ctx == Ctx::Keyframes || self.ctx == Ctx::Declarations {
            return Err(input.new_custom_error(()));
        }
        if lower == "namespace" {
            // recorded for the selectors after it, and dropped as a rule (see the module note)
            if self.ctx == Ctx::TopLevel && self.imports_allowed {
                if let Some(ns) = namespace_prelude(input) {
                    self.namespaces.push(ns);
                }
            }
            return Err(input.new_custom_error(()));
        }
        if lower == "import" {
            if !self.imports_allowed {
                return Err(input.new_custom_error(()));
            }
            return import_prelude(&mut self.sheet, input)
                .map(|i| AtPrelude::Import(Box::new(i)))
                .ok_or_else(|| input.new_custom_error(()));
        }
        let start = input.position();
        while input.next().is_ok() {}
        let prelude = input.slice(start..input.position());
        match lower.as_str() {
            "media" => {
                let text = collapse(prelude);
                Ok(AtPrelude::Media(self.sheet.push_media(MediaQueryList::parse(&text, None)), text))
            }
            "supports" => {
                let condition = collapse(prelude);
                if condition.is_empty() {
                    return Err(input.new_custom_error(()));
                }
                let matches = crate::style::supports::evaluate(&condition);
                Ok(AtPrelude::Supports(condition, matches))
            }
            "layer" => layer_names(prelude).map(AtPrelude::Layer).ok_or_else(|| input.new_custom_error(())),
            // Both of the descriptor at-rules take exactly one identifier, and a prelude that is not
            // one is not a rule at all — `@position-try p` names nothing, so the rule is dropped
            // whole rather than kept under a name no page could have written.
            "position-try" if self.ctx != Ctx::Style => at_rules::position_try_rule_name(prelude)
                .map(AtPrelude::PositionTry)
                .ok_or_else(|| input.new_custom_error(())),
            "counter-style" if self.ctx != Ctx::Style => at_rules::counter_style_rule_name(prelude)
                .map(AtPrelude::CounterStyle)
                .ok_or_else(|| input.new_custom_error(())),
            // `-webkit-keyframes` is not a browser name in engine source, it is a CSS at-rule a great
            // deal of the real web still ships; Chrome answers to it and so must this.
            "keyframes" | "-webkit-keyframes" if self.ctx != Ctx::Style => keyframes_rule_name(prelude)
                .map(AtPrelude::Keyframes)
                .ok_or_else(|| input.new_custom_error(())),
            "font-face" if self.ctx != Ctx::Style && prelude.trim().is_empty() => Ok(AtPrelude::FontFace),
            "container" => container_prelude(prelude)
                .map(|(n, q)| AtPrelude::Container(n, q))
                .ok_or_else(|| input.new_custom_error(())),
            "scope" => scope_prelude(prelude).map(|(a, b)| AtPrelude::Scope(a, b)).ok_or_else(|| input.new_custom_error(())),
            "property" if self.ctx != Ctx::Style => {
                let name = prelude.trim();
                if name.starts_with("--") && name.len() > 2 && !name.contains(char::is_whitespace) {
                    Ok(AtPrelude::Property(name.to_string()))
                } else {
                    Err(input.new_custom_error(()))
                }
            }
            "page" if self.ctx != Ctx::Style => Ok(AtPrelude::Page(collapse(prelude))),
            "starting-style" if prelude.trim().is_empty() => Ok(AtPrelude::StartingStyle),
            _ => Err(input.new_custom_error(())),
        }
    }

    fn rule_without_block(&mut self, prelude: AtPrelude, _start: &ParserState) -> Result<Item, ()> {
        match prelude {
            AtPrelude::Layer(names) if !names.is_empty() => {
                Ok(Item::Rule(Rule::new(RuleKind::LayerStatement { names })))
            }
            AtPrelude::Import(import) => Ok(Item::Rule(Rule::new(RuleKind::Import(import)))),
            _ => Err(()),
        }
    }

    fn parse_block<'t>(
        &mut self,
        prelude: AtPrelude,
        _start: &ParserState,
        input: &mut Parser<'i, 't>,
    ) -> Result<Item, ParseError<'i, ()>> {
        self.imports_allowed = false;
        let rule = match prelude {
            AtPrelude::Media(media, text) => {
                let children = self.group_children(input);
                Rule { children, ..Rule::new(RuleKind::Media { media, text }) }
            }
            AtPrelude::Supports(condition, matches) => {
                let children = self.group_children(input);
                Rule { children, ..Rule::new(RuleKind::Supports { condition, matches }) }
            }
            AtPrelude::Layer(names) => {
                if names.len() > 1 {
                    return Err(input.new_custom_error(()));
                }
                let children = self.group_children(input);
                let name = names.into_iter().next();
                Rule { children, ..Rule::new(RuleKind::LayerBlock { name, anon: anon_layer_id() }) }
            }
            AtPrelude::Import(_) => return Err(input.new_custom_error(())),
            AtPrelude::PositionTry(name) => {
                let block = at_rules::build_position_try_block(at_rules::read_descriptors(input));
                Rule::new(RuleKind::PositionTry { name, block })
            }
            AtPrelude::CounterStyle(name) => {
                let block = at_rules::build_counter_style_block(at_rules::read_descriptors(input));
                Rule::new(RuleKind::CounterStyle { name, block })
            }
            AtPrelude::Keyframes(name) => {
                // The blocks inside are qualified rules like any others -- the difference is the
                // prelude, which is a keyframe selector and not a CSS selector.
                let outer = std::mem::replace(&mut self.ctx, Ctx::Keyframes);
                let children = self.rule_list(input);
                self.ctx = outer;
                // An EMPTY `@keyframes` is still a rule in the CSSOM, but it drives no animation --
                // Chrome creates no `CSSAnimation` for `animation-name` pointing at one (measured:
                // `animation: nosuchkeyframes 4s` leaves `getAnimations()` empty).
                Rule { children, ..Rule::new(RuleKind::Keyframes { name }) }
            }
            AtPrelude::FontFace => {
                let mut block = DescriptorBlock::default();
                for (name, value) in at_rules::read_descriptors(input) {
                    if let Some(value) = at_rules::font_face_descriptor(&name, &value) {
                        block.set(&name, value);
                    }
                }
                Rule::new(RuleKind::FontFace { block })
            }
            AtPrelude::Container(name, query) => {
                let children = self.group_children(input);
                Rule { children, ..Rule::new(RuleKind::Container { name, query }) }
            }
            AtPrelude::Scope(start, end) => {
                let children = self.group_children(input);
                Rule { children, ..Rule::new(RuleKind::Scope { start, end }) }
            }
            AtPrelude::StartingStyle => {
                let children = self.group_children(input);
                Rule { children, ..Rule::new(RuleKind::StartingStyle) }
            }
            // An `@property` that does not register -- a missing `syntax` or `inherits`, an
            // `initial-value` missing where the syntax is not `*` or not computationally independent --
            // is not a rule at all: Chrome 150 leaves `@property --bad { syntax: "<length>"; inherits:
            // false }` out of `cssRules`.
            AtPrelude::Property(name) => {
                let block = property_block(at_rules::read_descriptors(input));
                if crate::style::registered::Registration::from_block(&block).is_none() {
                    return Err(input.new_custom_error(()));
                }
                Rule::new(RuleKind::Property { name, block })
            }
            AtPrelude::Page(selector) => {
                let mut block = DescriptorBlock::default();
                for (name, value) in at_rules::read_descriptors(input) {
                    block.set(&name, value);
                }
                Rule::new(RuleKind::Page { selector, block })
            }
        };
        Ok(Item::Rule(rule))
    }
}

impl<'i> RuleBodyItemParser<'i, Item, ()> for Builder {
    fn parse_declarations(&self) -> bool {
        matches!(self.ctx, Ctx::Style | Ctx::Declarations)
    }
    /// Nested rules only inside a style rule (CSS Nesting 1).
    fn parse_qualified(&self) -> bool {
        self.ctx == Ctx::Style
    }
}

/// The span of `text` with leading and trailing whitespace removed, as absolute byte offsets.
///
/// The value slice is taken between two tokenizer positions, so it can carry the whitespace that
/// sat before the delimiter. Trimming here rather than at read time means the stored span is the
/// value and nothing else — `getComputedStyle` should not be handing back a trailing space.
fn trim_span(text: &str, base: usize) -> Span {
    let trimmed = text.trim_matches(|c: char| c.is_ascii_whitespace());
    if trimmed.is_empty() {
        return Span::EMPTY;
    }
    let offset = trimmed.as_ptr() as usize - text.as_ptr() as usize;
    Span::of(base + offset..base + offset + trimmed.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::media::MediaContext;
    use crate::style::rules::LayerPath;
    use crate::style::rules::Origin;

    /// Every (property, value, important) triple in a sheet, flattened, for compact assertions.
    fn decls(sheet: &Stylesheet) -> Vec<(&'static str, String, bool)> {
        sheet
            .cascade_rules()
            .iter()
            .flat_map(|r| sheet.declarations_of(r))
            .map(|d| (d.property.name(), sheet.value_of(d).to_string(), d.important))
            .collect()
    }

    /// The selectors the cascade matches, one entry per rule, resolved.
    fn selectors(sheet: &Stylesheet) -> Vec<String> {
        sheet
            .cascade_rules()
            .iter()
            .map(|r| r.selectors.iter().map(|s| s.to_css_string()).collect::<Vec<_>>().join(", "))
            .collect()
    }

    #[test]
    fn a_rule_becomes_a_selector_and_its_declarations() {
        let sheet = parse_stylesheet(Origin::Author, "p { color: red; margin-top: 4px }");
        assert_eq!(sheet.rules().len(), 1);
        assert_eq!(
            decls(&sheet),
            vec![("color", "red".into(), false), ("margin-top", "4px".into(), false)]
        );
    }

    #[test]
    fn a_selector_list_is_one_rule_with_two_selectors() {
        let sheet = parse_stylesheet(Origin::Author, "h1, .lede { color: red }");
        assert_eq!(sheet.rules().len(), 1, "cssRules sees one rule here");
        assert_eq!(sheet.rules()[0].selector_text(), "h1, .lede");
    }

    #[test]
    fn document_order_is_the_order_the_rules_are_in() {
        let sheet = parse_stylesheet(Origin::Author, "a { color: red } b { color: blue } i { color: teal }");
        assert_eq!(selectors(&sheet), vec!["a", "b", "i"]);
    }

    // The specified behaviour, not leniency: a page using one property we do not know must still
    // get the rest of its sheet.
    #[test]
    fn a_bad_declaration_is_dropped_and_the_block_carries_on() {
        let sheet = parse_stylesheet(Origin::Author, "p { color: red; nonsense: 1; margin-top: 4px }");
        assert_eq!(
            decls(&sheet),
            vec![("color", "red".into(), false), ("margin-top", "4px".into(), false)]
        );
    }

    #[test]
    fn a_bad_selector_drops_its_rule_and_the_sheet_carries_on() {
        let sheet = parse_stylesheet(Origin::Author, "p!!! { color: red } p { color: blue }");
        assert_eq!(sheet.rules().len(), 1);
        assert_eq!(decls(&sheet), vec![("color", "blue".into(), false)]);
    }

    // An unclosed block is not a *malformed* sheet, it is a sheet whose block runs to EOF: the
    // tokenizer closes it there, so everything after `((` is inside it. Chrome does the same.
    #[test]
    fn an_unclosed_block_swallows_the_rest_of_the_sheet_as_the_tokenizer_says_it_should() {
        let sheet = parse_stylesheet(Origin::Author, "(( { color: red } p { color: blue }");
        assert_eq!(sheet.cascade_rules().len(), 0);
    }

    #[test]
    fn an_unterminated_rule_does_not_eat_the_rest_of_the_sheet() {
        let sheet = parse_stylesheet(Origin::Author, "p { color: red\nb { color: blue }");
        // `b { color: blue }` is read as part of p's block and dropped, but nothing panics and the
        // sheet still parses -- which is the whole contract for malformed CSS.
        assert!(sheet.rules().len() <= 2);
    }

    #[test]
    fn important_is_recorded_and_kept_out_of_the_value() {
        let sheet = parse_stylesheet(Origin::Author, "p { color: red !important; width: 1px }");
        assert_eq!(
            decls(&sheet),
            vec![("color", "red".into(), true), ("width", "1px".into(), false)]
        );
    }

    #[test]
    fn important_is_case_insensitive_and_may_be_spaced() {
        let sheet = parse_stylesheet(Origin::Author, "p { color: red ! IMPORTANT }");
        assert_eq!(decls(&sheet), vec![("color", "red".into(), true)]);
    }

    // A `!` that is not `!important` invalidates the declaration rather than being ignored.
    #[test]
    fn a_bang_that_is_not_important_invalidates_the_declaration() {
        let sheet = parse_stylesheet(Origin::Author, "p { color: red !importnt; width: 1px }");
        assert_eq!(decls(&sheet), vec![("width", "1px".into(), false)]);
    }

    // The reason `!important` is found by delimiting rather than by scanning the text.
    #[test]
    fn a_bang_inside_a_string_or_a_function_is_not_a_delimiter() {
        let sheet = parse_stylesheet(Origin::Author, r#"p { content: "!important"; width: calc(1px + 2px) }"#);
        assert_eq!(
            decls(&sheet),
            vec![("content", "\"!important\"".into(), false), ("width", "calc(1px + 2px)".into(), false)],
            "the `!` inside the string is not a delimiter, and calc survives intact"
        );
    }

    #[test]
    fn values_keep_their_internal_spacing_and_lose_the_surrounding_kind() {
        let sheet = parse_stylesheet(Origin::Author, "p {   margin-top :   4px   ;   width:1px  }");
        assert_eq!(
            decls(&sheet),
            vec![("margin-top", "4px".into(), false), ("width", "1px".into(), false)]
        );
        let sheet = parse_stylesheet(Origin::Author, "p { grid-template-columns: 1fr  2fr }");
        assert_eq!(decls(&sheet), vec![("grid-template-columns", "1fr  2fr".into(), false)]);
    }

    #[test]
    fn property_names_are_case_insensitive() {
        let sheet = parse_stylesheet(Origin::Author, "p { COLOR: red; Margin-Top: 1px }");
        assert_eq!(
            decls(&sheet),
            vec![("color", "red".into(), false), ("margin-top", "1px".into(), false)]
        );
    }

    #[test]
    fn a_declaration_with_no_value_declares_nothing() {
        let sheet = parse_stylesheet(Origin::Author, "p { color: ; width: 1px }");
        assert_eq!(decls(&sheet), vec![("width", "1px".into(), false)]);
    }

    #[test]
    fn an_empty_block_is_still_a_rule() {
        let sheet = parse_stylesheet(Origin::Author, "p { }");
        assert_eq!(sheet.rules().len(), 1);
        assert!(sheet.declarations_of(&sheet.rules()[0]).is_empty());
    }

    #[test]
    fn comments_and_blank_input_are_not_rules() {
        assert_eq!(parse_stylesheet(Origin::Author, "").rules().len(), 0);
        assert_eq!(parse_stylesheet(Origin::Author, "   \n\t ").rules().len(), 0);
        assert_eq!(parse_stylesheet(Origin::Author, "/* nothing here */").rules().len(), 0);
    }

    // Named so it fails loudly when shorthands land: today this is a hole, not a decision.
    #[test]
    fn a_shorthand_is_stored_as_its_longhands() {
        let sheet = parse_stylesheet(Origin::Author, "p { margin: 0 auto; color: red }");
        assert_eq!(
            decls(&sheet),
            vec![
                ("margin-top", "0".into(), false),
                ("margin-right", "auto".into(), false),
                ("margin-bottom", "0".into(), false),
                ("margin-left", "auto".into(), false),
                ("color", "red".into(), false),
            ]
        );
    }

    #[test]
    fn important_carries_onto_every_longhand_a_shorthand_expands_to() {
        let sheet = parse_stylesheet(Origin::Author, "p { padding: 1px !important }");
        assert!(decls(&sheet).iter().all(|d| d.2), "all four are important");
        assert_eq!(decls(&sheet).len(), 4);
    }

    // An invalid shorthand contributes nothing, rather than contributing nonsense to each longhand.
    #[test]
    fn an_invalid_shorthand_expands_to_nothing() {
        let sheet = parse_stylesheet(Origin::Author, "p { margin: 4px red; color: red }");
        assert_eq!(decls(&sheet), vec![("color", "red".into(), false)]);
    }

    // `padding: initial` is every padding longhand set to `initial`, not an invalid padding
    // (Chromium's UA sheet resets checkbox and radio padding and border this way).
    #[test]
    fn a_css_wide_keyword_reaches_every_longhand_of_a_shorthand() {
        let sheet = parse_stylesheet(Origin::Author, "p { padding: initial; border: unset }");
        let d = decls(&sheet);
        // `border` is seventeen longhands in Blink, its five `border-image-*` among them (`css_properties.json5`;
        // Chrome 150 lists `style="border: 0"` as seventeen, `fixtures/css-answers/inline-style-chrome-150.json`)
        assert_eq!(d.len(), 4 + 17, "{d:?}");
        assert!(d[..4].iter().all(|(n, v, _)| n.starts_with("padding-") && v == "initial"));
        assert!(d[4..].iter().all(|(n, v, _)| n.starts_with("border-") && v == "unset"));
    }

    // `all` is every longhand but `direction` and `unicode-bidi`, and takes nothing but a CSS-wide keyword
    #[test]
    fn all_reaches_every_longhand_but_direction_and_unicode_bidi() {
        let sheet = parse_stylesheet(Origin::Author, "p { all: unset } q { all: 0 }");
        let d = decls(&sheet);
        assert!(d.len() > 200, "{}", d.len());
        assert!(d.iter().all(|(_, v, _)| v == "unset"));
        for name in ["color", "display", "font-size", "margin-top", "text-decoration-line"] {
            assert!(d.iter().any(|(n, _, _)| *n == name), "{name}");
        }
        assert!(!d.iter().any(|(n, _, _)| *n == "direction" || *n == "unicode-bidi"));
    }

    #[test]
    fn a_compound_shorthand_fills_the_longhands_it_omits() {
        let sheet = parse_stylesheet(Origin::Author, "p { border-top: solid }");
        assert_eq!(
            decls(&sheet),
            vec![
                ("border-top-width", "medium".into(), false),
                ("border-top-style", "solid".into(), false),
                ("border-top-color", "currentcolor".into(), false),
            ],
            "the omitted width and color come from values the sheet never wrote"
        );
    }

    #[test]
    fn every_shorthand_a_real_page_uses_reaches_its_longhands() {
        let sheet = parse_stylesheet(
            Origin::Author,
            "p { margin: 0 auto; border: 1px solid red; font: 12px serif; flex: 1;
                 background: url(a.png) no-repeat; border-radius: 4px; white-space: nowrap }",
        );
        let names: Vec<&str> = decls(&sheet).iter().map(|d| d.0).collect();
        for expected in [
            "margin-top", "border-top-style", "font-size", "font-family", "flex-basis",
            "background-image", "border-top-left-radius", "text-wrap-mode",
        ] {
            assert!(names.contains(&expected), "{expected} missing from {names:?}");
        }
        assert!(
            names.iter().all(|n| PropertyId::from_name(n).is_some()),
            "everything stored is a longhand the table knows"
        );
    }

    // Same: at-rules this engine does not model are skipped whole, and skipping must not swallow
    // what follows them.
    #[test]
    fn an_unknown_at_rule_is_skipped_without_eating_the_next_rule() {
        let sheet = parse_stylesheet(
            Origin::Author,
            "@unknown-block (min-width: 1px) { p { color: red } } @charset \"utf-8\"; b { color: blue }",
        );
        assert_eq!(decls(&sheet), vec![("color", "blue".into(), false)]);
    }

    // The rules inside a `@media` are ordinary rules that carry a reference to the condition. They
    // are kept whether or not it currently matches: the CSSOM can see them, and a viewport change
    // makes them live without the sheet being parsed again.
    #[test]
    fn media_rules_are_kept_and_tagged_rather_than_filtered() {
        let sheet = parse_stylesheet(
            Origin::Author,
            "a { color: red } @media (min-width: 600px) { b { color: blue } } i { color: teal }",
        );
        assert_eq!(sheet.rules().len(), 3, "cssRules: a, the @media, i");
        assert!(matches!(sheet.rules()[1].kind, RuleKind::Media { .. }));
        let flat = sheet.cascade_rules();
        assert_eq!(flat.len(), 3);
        assert_eq!(flat[0].media, 0);
        assert_ne!(flat[1].media, 0);
        assert_eq!(flat[2].media, 0, "the condition ends with its block");
        let wide = MediaContext::default();
        let narrow = MediaContext { viewport_width: 400.0, ..MediaContext::default() };
        assert!(sheet.rule_applies(&flat[1], &wide));
        assert!(!sheet.rule_applies(&flat[1], &narrow));
        assert!(sheet.rule_applies(&flat[0], &narrow), "and the rest are unaffected");
    }

    #[test]
    fn a_nested_media_is_a_conjunction() {
        let sheet = parse_stylesheet(
            Origin::Author,
            "@media screen { @media (min-width: 600px) { p { color: red } } }",
        );
        assert_eq!(sheet.cascade_rules().len(), 1);
        let rule = &sheet.cascade_rules()[0];
        assert!(sheet.rule_applies(rule, &MediaContext::default()));
        assert!(!sheet.rule_applies(rule, &MediaContext { print: true, ..MediaContext::default() }));
        assert!(!sheet.rule_applies(
            rule,
            &MediaContext { viewport_width: 400.0, ..MediaContext::default() }
        ));
    }

    #[test]
    fn a_media_query_nobody_understands_hides_its_rules_without_dropping_them() {
        let sheet = parse_stylesheet(
            Origin::Author,
            "@media (min-inverted-colors: 1) { p { color: red } } b { color: blue }",
        );
        assert_eq!(sheet.cascade_rules().len(), 2);
        assert!(!sheet.rule_applies(&sheet.cascade_rules()[0], &MediaContext::default()));
        assert!(sheet.rule_applies(&sheet.cascade_rules()[1], &MediaContext::default()));
    }

    #[test]
    fn selectors_survive_the_round_trip() {
        let sheet = parse_stylesheet(Origin::Author, "#nav > li.active a[href] { color: red }");
        assert_eq!(selectors(&sheet), vec!["#nav > li.active a[href]"]);
    }

    #[test]
    fn a_style_attribute_is_a_sheet_with_declarations_and_no_rules() {
        let sheet = parse_inline_style("color: red; width: 3px !important");
        assert!(sheet.rules().is_empty());
        let got: Vec<(&str, &str, bool)> = sheet
            .declarations()
            .iter()
            .map(|d| (d.property.name(), sheet.value_of(d), d.important))
            .collect();
        assert_eq!(got, vec![("color", "red", false), ("width", "3px", true)]);
    }

    #[test]
    fn an_inline_style_expands_shorthands_too() {
        let sheet = parse_inline_style("margin: 0 auto");
        assert_eq!(sheet.declarations().len(), 4);
    }

    // ── `@keyframes` ────────────────────────────────────────────────────────────────────────────

    /// One keyframes rule as `name: off=prop:value,prop:value; …`, for compact assertions.
    fn kf(sheet: &Stylesheet) -> Vec<String> {
        sheet
            .keyframes()
            .iter()
            .map(|k| {
                let blocks: Vec<String> = k
                    .blocks
                    .iter()
                    .map(|b| {
                        let offsets: Vec<String> =
                            b.offsets.iter().map(|o| format!("{o}")).collect();
                        let decls: Vec<String> = sheet.declarations()
                            [b.decls.start as usize..b.decls.end as usize]
                            .iter()
                            .map(|d| format!("{}:{}", d.property.name(), sheet.value_of(d)))
                            .collect();
                        format!("{}={}", offsets.join("/"), decls.join(","))
                    })
                    .collect();
                format!("{}: {}", k.name, blocks.join("; "))
            })
            .collect()
    }

    #[test]
    fn keyframes_become_blocks_with_offsets_rather_than_vanishing() {
        let sheet = parse_stylesheet(
            Origin::Author,
            "@keyframes slide { from { opacity: 0 } to { opacity: 1 } }",
        );
        assert_eq!(kf(&sheet), vec!["slide: 0=opacity:0; 1=opacity:1"]);
        // …and it is NOT a `Rule`: a keyframes rule must never match an element.
        assert_eq!(sheet.cascade_rules().len(), 0, "no selector the cascade's index could match");
        assert!(matches!(sheet.rules()[0].kind, RuleKind::Keyframes { .. }), "but cssRules has a CSSKeyframesRule");
    }

    #[test]
    fn a_percentage_keyframe_selector_is_a_zero_to_one_offset() {
        let sheet = parse_stylesheet(
            Origin::Author,
            "@keyframes pct { 0% { width: 10px } 50% { width: 60px } 100% { width: 110px } }",
        );
        assert_eq!(kf(&sheet), vec!["pct: 0=width:10px; 0.5=width:60px; 1=width:110px"]);
    }

    #[test]
    fn one_block_may_name_several_offsets() {
        let sheet = parse_stylesheet(Origin::Author, "@keyframes b { 0%, 50% { opacity: 0 } to { opacity: 1 } }");
        assert_eq!(kf(&sheet), vec!["b: 0/0.5=opacity:0; 1=opacity:1"]);
    }

    #[test]
    fn the_webkit_prefixed_at_rule_is_the_same_at_rule() {
        let sheet = parse_stylesheet(Origin::Author, "@-webkit-keyframes spin { from { opacity: 0 } }");
        assert_eq!(kf(&sheet), vec!["spin: 0=opacity:0"]);
    }

    #[test]
    fn a_keyframes_name_keeps_its_case_and_none_is_not_one() {
        let sheet = parse_stylesheet(Origin::Author, "@keyframes Slide { from { opacity: 0 } }");
        assert_eq!(sheet.keyframes().len(), 1);
        assert_eq!(sheet.keyframes()[0].name, "Slide");
        // `animation-name: none` means "no animation", so a rule by that name is unreachable.
        let sheet = parse_stylesheet(Origin::Author, "@keyframes none { from { opacity: 0 } }");
        assert_eq!(sheet.keyframes().len(), 0);
    }

    #[test]
    fn a_bad_keyframe_selector_drops_its_block_and_keeps_the_rest() {
        let sheet = parse_stylesheet(
            Origin::Author,
            "@keyframes x { from { opacity: 0 } bogus { opacity: 0.5 } to { opacity: 1 } }",
        );
        assert_eq!(kf(&sheet), vec!["x: 0=opacity:0; 1=opacity:1"]);
    }

    #[test]
    fn the_last_keyframes_rule_of_a_name_wins() {
        let sheet = parse_stylesheet(
            Origin::Author,
            "@keyframes x { from { opacity: 0 } } @keyframes x { from { opacity: 0.4 } }",
        );
        assert_eq!(kf(&sheet), vec!["x: 0=opacity:0.4"]);
    }

    #[test]
    fn a_keyframes_rule_does_not_swallow_the_rule_after_it() {
        let sheet = parse_stylesheet(
            Origin::Author,
            "a { color: red } @keyframes x { from { opacity: 0 } } b { color: blue }",
        );
        assert_eq!(selectors(&sheet), vec!["a", "b"]);
        assert_eq!(sheet.keyframes().len(), 1);
    }

    #[test]
    fn a_keyframe_block_expands_shorthands_like_any_other_block() {
        let sheet = parse_stylesheet(Origin::Author, "@keyframes m { to { margin: 0 auto } }");
        assert_eq!(kf(&sheet), vec!["m: 1=margin-top:0,margin-right:auto,margin-bottom:0,margin-left:auto"]);
    }

    #[test]
    fn keyframes_inside_a_media_query_are_still_keyframes() {
        let sheet = parse_stylesheet(
            Origin::Author,
            "@media (min-width: 1px) { @keyframes x { from { opacity: 0 } } p { color: red } }",
        );
        assert_eq!(kf(&sheet), vec!["x: 0=opacity:0"]);
        assert_eq!(selectors(&sheet), vec!["p"], "the media block goes on being a media block");
    }

    // ── @supports, @layer, nesting, @import ──────────────────────────────────────────

    #[test]
    fn a_supports_block_applies_its_rules_only_when_the_condition_holds() {
        let sheet = parse_stylesheet(
            Origin::Author,
            "@supports (display: grid) { .s { color: red } } @supports (display: nope) { .f { color: blue } }",
        );
        assert_eq!(selectors(&sheet), vec![".s"]);
        assert_eq!(sheet.rules().len(), 2, "both are CSSSupportsRules in cssRules");
    }

    #[test]
    fn a_layer_block_is_a_layer_and_its_rules_carry_it() {
        let sheet = parse_stylesheet(
            Origin::Author,
            "@layer a, b.c; @layer { .x { color: red } } @layer a { .y { color: red } @layer inner { .z { color: red } } } .w { color: red }",
        );
        let flat = sheet.cascade_rules();
        assert_eq!(flat.len(), 4);
        let path = |i: usize| sheet.layers()[flat[i].layer as usize].clone();
        use crate::style::rules::LayerSeg;
        assert!(matches!(path(0).as_slice(), [LayerSeg::Anon(_)]));
        assert_eq!(path(1), vec![LayerSeg::Named("a".into())]);
        assert_eq!(path(2), vec![LayerSeg::Named("a".into()), LayerSeg::Named("inner".into())]);
        assert_eq!(flat[3].layer, 0, "an unlayered rule");
        // Declaration order: a, b, b.c (the statement), the anonymous one, a.inner.
        let names: Vec<LayerPath> = sheet.layers().to_vec();
        assert_eq!(names[1], vec![LayerSeg::Named("a".into())]);
        assert_eq!(names[2], vec![LayerSeg::Named("b".into())]);
        assert_eq!(names[3], vec![LayerSeg::Named("b".into()), LayerSeg::Named("c".into())]);
    }

    #[test]
    fn a_nested_rule_resolves_against_its_parent_and_later_declarations_keep_their_place() {
        let sheet = parse_stylesheet(
            Origin::Author,
            ".p { color: red; & .c { color: blue } > .d { color: green } color: teal; .e & { color: pink } }",
        );
        assert_eq!(selectors(&sheet), vec![".p", ":is(.p) .c", ":is(.p) > .d", ".p", ".e :is(.p)"]);
        let top = &sheet.rules()[0];
        assert_eq!(top.children.len(), 4, "& .c, > .d, the nested declarations, .e &");
        assert_eq!(top.children[0].selector_text(), "& .c");
        assert_eq!(top.children[1].selector_text(), "& > .d");
        assert!(matches!(top.children[2].kind, RuleKind::NestedDeclarations));
        assert_eq!(decls(&sheet)[0], ("color", "red".into(), false));
        assert_eq!(decls(&sheet)[3], ("color", "teal".into(), false));
    }

    #[test]
    fn an_import_is_a_rule_before_the_others_and_nothing_after_them() {
        let sheet = parse_stylesheet(
            Origin::Author,
            "@import url(a.css); @import 'b.css' print; @import url(c.css) layer(l) supports(display: grid) screen; @import url(d.css) supports(display: nope); .x {} @import url(e.css);",
        );
        let kinds: Vec<String> = sheet
            .rules()
            .iter()
            .map(|r| match &r.kind {
                RuleKind::Import(i) => format!("import {} {:?} {:?} {:?}", i.href, i.media.as_ref().map(|m| &m.1), i.layer, i.supports),
                RuleKind::Style => "style".to_string(),
                _ => "other".to_string(),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "import a.css None None None".to_string(),
                "import b.css Some(\"print\") None None".to_string(),
                "import c.css Some(\"screen\") Some(\"l\") Some(\"display: grid\")".to_string(),
                "style".to_string(),
            ],
            "the unmet supports() import is dropped, and so is one after a style rule"
        );
    }
}
