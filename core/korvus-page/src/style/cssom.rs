//! The CSSOM's view of a parsed sheet: serializing rules back to CSS, and editing them in place.
//!
//! This is the half of a stylesheet that pages *touch*. It matters more than it looks:
//! CSS-in-JS libraries — emotion, styled-components, MUI, and therefore most React and Next.js
//! apps — style by calling `styleEl.sheet.insertRule(...)` during render. A `.sheet` that is
//! missing or that throws takes the client render down with it, and Next.js turns that into a blank
//! page. So these operations have to exist, behave, and above all not throw where a browser would
//! not.
//!
//! # Rules are addressed by PATH
//!
//! A rule is `path = [i, j, k]`: the `k`th child of the `j`th child of the sheet's `i`th rule. The
//! top-level list is the empty path. `@media`, `@supports`, `@layer`, `@import`, `@keyframes` and a
//! style rule with nested rules all hold children, and `CSSGroupingRule.insertRule` edits a child
//! list exactly as `CSSStyleSheet.insertRule` edits the top one. Every edit ends in
//! [`Stylesheet::rebuild`], which is what the cascade reads.
//!
//! # Serialization is specified, not "close enough"
//!
//! `cssRules[0].cssText` is a string the page can read back and compare. Chrome writes
//! `p { color: red; }` — one space inside each brace, `; ` between declarations, and a trailing
//! semicolon that is there even after the last one. A grouping rule writes each child on its own
//! line indented by two spaces (Blink's `CSSGroupingRule::AppendCSSTextForItems`), and `@keyframes`
//! writes `" { \n"` with the stray space Chrome has. None of that is cosmetic when a test asserts
//! equality. Values are still echoed as the page wrote them rather than normalized (`color: #f00`
//! stays `#f00` where Chrome answers `rgb(255, 0, 0)`).

use crate::style::at_rules;
use crate::style::parse::{parse_rule_in, parse_stylesheet, InsertContext};
use crate::style::rules::{Rule, RuleKind, Span, Stylesheet};

/// A parsed declaration list made a property set as `CreateCSSPropertyValueSet` makes one: kept as
/// parsed when it holds fewer than two declarations, or two different ones -- Chrome 150 writes `.r { color: red
/// !important; width: 0 }` back in that order -- and otherwise `FilterProperties`'d: `!important` last, the last of
/// a property winning (two custom properties are one property id there, so never the fast path).
pub(crate) fn as_property_set(entries: Vec<crate::style::serialize::Entry>) -> Vec<crate::style::serialize::Entry> {
    let same = |a: &crate::style::serialize::Entry, b: &crate::style::serialize::Entry| {
        a.name == b.name || (a.name.starts_with("--") && b.name.starts_with("--"))
    };
    let fast = entries.len() < 2 || (entries.len() == 2 && !same(&entries[0], &entries[1]));
    if fast {
        entries
    } else {
        crate::style::serialize::filter(entries)
    }
}
use korvus_dom::Selector;

/// What `cssRules[i]` is, as one number the webapi side switches on to pick the interface.
///
/// Not `CSSRule.type`: that legacy numbering stopped being extended years ago and every rule added
/// since reports `0`, so it cannot tell a `@layer` from a `@counter-style`.
pub const RULE_STYLE: u32 = 0;
pub const RULE_POSITION_TRY: u32 = 1;
pub const RULE_COUNTER_STYLE: u32 = 2;
pub const RULE_MEDIA: u32 = 3;
pub const RULE_SUPPORTS: u32 = 4;
pub const RULE_LAYER_BLOCK: u32 = 5;
pub const RULE_LAYER_STATEMENT: u32 = 6;
pub const RULE_IMPORT: u32 = 7;
pub const RULE_KEYFRAMES: u32 = 8;
pub const RULE_KEYFRAME: u32 = 9;
pub const RULE_FONT_FACE: u32 = 10;
pub const RULE_NESTED_DECLARATIONS: u32 = 11;
pub const RULE_CONTAINER: u32 = 12;
pub const RULE_SCOPE: u32 = 13;
pub const RULE_PROPERTY: u32 = 14;
pub const RULE_PAGE: u32 = 15;
pub const RULE_STARTING_STYLE: u32 = 16;

/// The failures the CSSOM turns into DOM exceptions. Nothing else here can fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CssomError {
    /// The index is past the end — `IndexSizeError`.
    IndexSize,
    /// The text is not a rule — `SyntaxError`.
    Syntax,
    /// The rule is not allowed where it would land (an `@import` after a style rule, a style rule
    /// before an `@import`) — `HierarchyRequestError`.
    Hierarchy,
}

/// A 0..1 keyframe offset as `keyText` writes it: `0%`, `50%`, `100%`.
fn percent(offset: f32) -> String {
    let p = (offset * 100.0 * 1000.0).round() / 1000.0;
    format!("{p}%")
}

/// `url(x)` -> `url("x")`, the way Chrome serializes a URL inside a descriptor.
fn quote_urls(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 4);
    let mut rest = value;
    while let Some(i) = rest.find("url(") {
        out.push_str(&rest[..i + 4]);
        rest = &rest[i + 4..];
        let Some(end) = rest.find(')') else { break };
        let inner = rest[..end].trim();
        if inner.starts_with('"') || inner.starts_with('\'') {
            out.push_str(&rest[..end]);
        } else {
            out.push('"');
            out.push_str(inner);
            out.push('"');
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

impl Stylesheet {
    /// A declaration block's `cssText`: `color: red; width: 1px;`.
    ///
    /// The trailing semicolon is on the last declaration too. An empty block is the empty string,
    /// not `" "` — a page that checks `style.cssText === ""` is asking whether anything is set.
    ///
    /// In Blink's spelling (`style::serialize`): each value as `CSSValue::CssText`
    /// writes it, `!important` declarations after the others, a property at its last occurrence, and every
    /// shorthand whose longhands are all here written as the shorthand.
    pub fn declaration_block_text(&self, decls: Span) -> String {
        crate::style::serialize::block_text(&as_property_set(self.serializer_entries(decls)))
    }

    /// A style rule's declaration block as Blink holds it: its longhands, with its text-kept declarations (custom
    /// properties, values holding `var()`) where they were written among them, made a property set
    /// ([`as_property_set`]). Chrome 150 writes `.r { --x: 1px; color: red }` back as `--x: 1px; color: red;`, length
    /// 2, `item(0)` `--x`; this wrote `color: red;` and listed `--x` last.
    pub fn style_block_entries(&self, rule: &Rule) -> Vec<crate::style::serialize::Entry> {
        let mut entries = self.serializer_entries(rule.decls);
        for (shift, r) in self.raw_of(rule).iter().enumerate() {
            let name = self.slice(r.name);
            let name = if name.starts_with("--") { name.to_string() } else { name.to_ascii_lowercase() };
            let entry = crate::style::serialize::Entry {
                name,
                value: self.slice(r.value).trim().to_string(),
                important: r.important,
                implicit: false,
            };
            let at = (r.after as usize + shift).min(entries.len());
            entries.insert(at, entry);
        }
        as_property_set(entries)
    }

    /// [`Stylesheet::style_block_entries`] serialized: a style rule's `style.cssText`.
    pub fn style_block_text(&self, rule: &Rule) -> String {
        crate::style::serialize::block_text(&self.style_block_entries(rule))
    }

    /// The declarations of `decls` as the serializer reads them.
    pub fn serializer_entries(&self, decls: Span) -> Vec<crate::style::serialize::Entry> {
        (decls.start as usize..decls.end as usize)
            .map(|i| {
                let d = &self.declarations()[i];
                let name = d.property.name();
                crate::style::serialize::Entry {
                    name: name.to_string(),
                    value: if d.implicit { "initial".into() } else { crate::style::serialize::value_text(name, self.value_of(d)) },
                    important: d.important,
                    implicit: d.implicit,
                }
            })
            .collect()
    }

    /// A rule's `cssText`, in Chrome's exact spelling for each kind.
    pub fn rule_css_text(&self, rule: &Rule) -> String {
        // `@media x {\n  child\n  child\n}` -- each child indented two spaces on its own line.
        let group = |head: String| {
            let mut out = head;
            out.push_str(" {\n");
            for child in &rule.children {
                let t = self.rule_css_text(child);
                if !t.is_empty() {
                    out.push_str("  ");
                    out.push_str(&t);
                    out.push('\n');
                }
            }
            out.push('}');
            out
        };
        let braces = |head: String, body: String| {
            if body.is_empty() {
                format!("{head} {{ }}")
            } else {
                format!("{head} {{ {body} }}")
            }
        };
        match &rule.kind {
            RuleKind::Style => {
                let decls = self.style_block_text(rule);
                let children: Vec<String> =
                    rule.children.iter().map(|c| self.rule_css_text(c)).filter(|t| !t.is_empty()).collect();
                if children.is_empty() {
                    return braces(rule.selector_text(), decls);
                }
                // CSSOM "serialize a CSS rule" for a style rule with child rules (Blink's
                // `CSSStyleRule::cssText`): the declarations on one line, then each child.
                let mut out = format!("{} {{", rule.selector_text());
                if !decls.is_empty() {
                    out.push_str("\n  ");
                    out.push_str(&decls);
                }
                for c in children {
                    out.push_str("\n  ");
                    out.push_str(&c);
                }
                out.push_str("\n}");
                out
            }
            RuleKind::NestedDeclarations => self.style_block_text(rule),
            RuleKind::PositionTry { name, block } => braces(format!("@position-try {name}"), block.css_text()),
            RuleKind::CounterStyle { name, block } => braces(format!("@counter-style {name}"), block.css_text()),
            RuleKind::FontFace { block } => braces("@font-face".to_string(), quote_urls(&block.css_text())),
            RuleKind::Media { text, .. } => group(format!("@media {}", crate::style::serialize::media_text(text))),
            RuleKind::Supports { condition, .. } => group(format!("@supports {condition}")),
            RuleKind::LayerBlock { name, .. } => group(match name {
                Some(n) => format!("@layer {n}"),
                None => "@layer".to_string(),
            }),
            RuleKind::LayerStatement { names } => format!("@layer {};", names.join(", ")),
            RuleKind::Import(import) => {
                let mut out = format!("@import url(\"{}\")", import.href);
                if let Some(layer) = &import.layer {
                    out.push_str(" layer");
                    if !layer.is_empty() {
                        out.push_str(&format!("({layer})"));
                    }
                }
                if let Some(s) = &import.supports {
                    out.push_str(&format!(" supports({s})"));
                }
                if let Some((_, m)) = &import.media {
                    out.push(' ');
                    out.push_str(m);
                }
                out.push(';');
                out
            }
            RuleKind::Keyframes { name } => {
                let mut out = format!("@keyframes {name} {{ \n");
                for child in &rule.children {
                    out.push_str("  ");
                    out.push_str(&self.rule_css_text(child));
                    out.push('\n');
                }
                out.push('}');
                out
            }
            RuleKind::Keyframe { offsets } => braces(key_text(offsets), self.declaration_block_text(rule.decls)),
            // Chrome 150 (the unit's `atrules.html`):
            //   `@container (min-width: 200px) {\n  .cqc { color: rgb(9, 9, 9); }\n}`
            //   `@scope (.sc2) to (.stop) {\n  p { color: rgb(18, 18, 18); }\n}`
            //   `@property --x { syntax: "<length>"; inherits: false; initial-value: 7px; }`
            //   `@page :first { margin: 1in; }`
            RuleKind::Container { name, query } => group(match name {
                Some(n) => format!("@container {n} {query}"),
                None => format!("@container {query}"),
            }),
            RuleKind::Scope { start, end } => {
                let mut head = "@scope".to_string();
                if let Some(s) = start {
                    head.push_str(&format!(" ({s})"));
                }
                if let Some(e) = end {
                    head.push_str(&format!(" to ({e})"));
                }
                group(head)
            }
            RuleKind::StartingStyle => group("@starting-style".to_string()),
            RuleKind::Property { name, block } => braces(format!("@property {name}"), block.css_text()),
            RuleKind::Page { selector, block } => braces(
                if selector.is_empty() { "@page".to_string() } else { format!("@page {selector}") },
                block.css_text(),
            ),
        }
    }

    /// How many rules are in the list at `path` (the top level for an empty path). `None` when the
    /// path does not address a list.
    pub fn list_len(&self, path: &[u32]) -> Option<usize> {
        self.list_at(path).map(|l| l.len())
    }

    /// `cssText` of the rule at `path`, or `""`.
    pub fn css_text_at(&self, path: &[u32]) -> String {
        self.rule_at(path).map(|r| self.rule_css_text(r)).unwrap_or_default()
    }

    /// `selectorText` of the rule at `path`, or `""`.
    pub fn selector_text_at(&self, path: &[u32]) -> String {
        self.rule_at(path).map(Rule::selector_text).unwrap_or_default()
    }

    /// Which interface the rule at `path` is.
    pub fn rule_kind_code(&self, path: &[u32]) -> u32 {
        match self.rule_at(path).map(|r| &r.kind) {
            Some(RuleKind::PositionTry { .. }) => RULE_POSITION_TRY,
            Some(RuleKind::CounterStyle { .. }) => RULE_COUNTER_STYLE,
            Some(RuleKind::Media { .. }) => RULE_MEDIA,
            Some(RuleKind::Supports { .. }) => RULE_SUPPORTS,
            Some(RuleKind::LayerBlock { .. }) => RULE_LAYER_BLOCK,
            Some(RuleKind::LayerStatement { .. }) => RULE_LAYER_STATEMENT,
            Some(RuleKind::Import(_)) => RULE_IMPORT,
            Some(RuleKind::Keyframes { .. }) => RULE_KEYFRAMES,
            Some(RuleKind::Keyframe { .. }) => RULE_KEYFRAME,
            Some(RuleKind::FontFace { .. }) => RULE_FONT_FACE,
            Some(RuleKind::NestedDeclarations) => RULE_NESTED_DECLARATIONS,
            Some(RuleKind::Container { .. }) => RULE_CONTAINER,
            Some(RuleKind::Scope { .. }) => RULE_SCOPE,
            Some(RuleKind::Property { .. }) => RULE_PROPERTY,
            Some(RuleKind::Page { .. }) => RULE_PAGE,
            Some(RuleKind::StartingStyle) => RULE_STARTING_STYLE,
            _ => RULE_STYLE,
        }
    }

    /// An at-rule's `name` (`@layer` block, `@keyframes`, `@position-try`, `@counter-style`).
    pub fn at_rule_name(&self, path: &[u32]) -> String {
        self.rule_at(path).and_then(|r| r.kind.name()).unwrap_or_default().to_string()
    }

    /// `name`'s setter. `@counter-style` and `@keyframes` have one -- `CSSPositionTryRule.name` and
    /// `CSSLayerBlockRule.name` are readonly -- and a name that does not fit the grammar leaves the
    /// rule alone rather than throwing.
    pub fn set_at_rule_name(&mut self, path: &[u32], text: &str) -> bool {
        let Some(rule) = self.rule_at_mut(path) else { return false };
        let changed = match &mut rule.kind {
            RuleKind::CounterStyle { name, .. } => match at_rules::counter_style_rule_name(text) {
                Some(parsed) => {
                    *name = parsed;
                    true
                }
                None => false,
            },
            RuleKind::Keyframes { name } if !text.is_empty() => {
                *name = text.to_string();
                true
            }
            _ => false,
        };
        if changed {
            self.rebuild();
        }
        changed
    }

    /// Read one descriptor off an at-rule's block. `""` for anything that is not set, is not a
    /// descriptor of this rule, or is not an at-rule at all — a browser answers the empty string to
    /// all three.
    pub fn descriptor(&self, path: &[u32], name: &str) -> String {
        match self.rule_at(path).map(|r| &r.kind) {
            Some(RuleKind::PositionTry { block, .. }) => at_rules::position_try_get(block, name),
            Some(RuleKind::CounterStyle { block, .. }) => at_rules::counter_style_get(block, name),
            Some(RuleKind::FontFace { block }) | Some(RuleKind::Page { block, .. }) | Some(RuleKind::Property { block, .. }) => {
                block.get(name).to_string()
            }
            _ => String::new(),
        }
    }

    /// The property names of the rule's declaration block, in order -- what `rule.style.length` counts
    /// and `rule.style.item(i)` lists (a shorthand as its longhands, a custom property by its name).
    /// Chrome 150: `a{color:red;width:3px}` is length 2, `item(1)` `width`.
    pub fn rule_property_names(&self, path: &[u32]) -> Vec<String> {
        let Some(rule) = self.rule_at(path) else { return Vec::new() };
        if let Some(block) = rule.kind.block() {
            return block.entries().iter().map(|(k, _)| k.clone()).collect();
        }
        // the property set's own order: a custom property where it was written, `!important` last in
        // a filtered block -- the block `cssText` writes
        let mut out: Vec<String> = Vec::new();
        for e in self.style_block_entries(rule) {
            if !out.contains(&e.name) {
                out.push(e.name);
            }
        }
        out
    }

    /// One property of the declaration block of the rule at `path`, as `rule.style[name]` reads it.
    /// A descriptor at-rule answers its descriptor as written (URLs quoted, as Chrome
    /// serializes `@font-face`'s `src`); a style rule its last declaration of a longhand, a four-side
    /// shorthand whose longhands are all set, or a custom property.
    pub fn rule_property_value(&self, path: &[u32], name: &str) -> String {
        use crate::style::property::PropertyId;
        let Some(rule) = self.rule_at(path) else { return String::new() };
        if let Some(block) = rule.kind.block() {
            let v = block.get(name);
            return if matches!(rule.kind, RuleKind::FontFace { .. }) { quote_urls(v) } else { v.to_string() };
        }
        if name.starts_with("--") {
            return self
                .raw_of(rule)
                .iter()
                .rev()
                .find(|d| self.slice(d.name) == name)
                .map(|d| self.slice(d.value).to_string())
                .unwrap_or_default();
        }
        let decls = self.declarations_of(rule);
        // a longhand's value as `CSSValue::CssText` writes it: `margin: 0` reads `margin-top` back as
        // `0px` in Chrome 150, where this handed back the `0` the shorthand was written with
        let longhand = |p: PropertyId| {
            decls.iter().rev().find(|d| d.property == p).map(|d| {
                if d.implicit {
                    "initial".to_string()
                } else {
                    crate::style::serialize::value_text(p.name(), self.value_of(d))
                }
            })
        };
        let canonical = crate::style::property::canonical_name(name);
        let name = canonical.as_str();
        // a declaration kept as text under that name: Chrome 150 reads `.r { width: var(--w) }`'s `width` back as
        // `var(--w)` (and a shorthand's the same way), where this read ""
        if let Some(r) = self.raw_of(rule).iter().rev().find(|r| self.slice(r.name).eq_ignore_ascii_case(name)) {
            if !decls.iter().any(|d| d.property.name() == name) {
                return self.slice(r.value).trim().to_string();
            }
        }
        // A shorthand as the block's longhands write it (`style::serialize`): `background-position` among them, which
        // the cascade keeps as its two halves.
        if crate::style::serialize::is_shorthand_name(name) && !matches!(name, "margin" | "padding" | "inset" | "border-width" | "border-style" | "border-color") {
            let entries = crate::style::serialize::filter(self.serializer_entries(crate::style::rules::Declares::decl_span(rule)));
            return crate::style::serialize::shorthand_value(name, &entries).unwrap_or_default();
        }
        if let Some(p) = PropertyId::from_name(name) {
            return longhand(p).unwrap_or_default();
        }
        use PropertyId::*;
        let sides: Option<[PropertyId; 4]> = match name {
            "margin" => Some([MarginTop, MarginRight, MarginBottom, MarginLeft]),
            "padding" => Some([PaddingTop, PaddingRight, PaddingBottom, PaddingLeft]),
            "inset" => Some([Top, Right, Bottom, Left]),
            "border-width" => Some([BorderTopWidth, BorderRightWidth, BorderBottomWidth, BorderLeftWidth]),
            "border-style" => Some([BorderTopStyle, BorderRightStyle, BorderBottomStyle, BorderLeftStyle]),
            "border-color" => Some([BorderTopColor, BorderRightColor, BorderBottomColor, BorderLeftColor]),
            _ => None,
        };
        let Some(sides) = sides else { return String::new() };
        let values: Option<Vec<String>> = sides.iter().map(|p| longhand(*p)).collect();
        let Some(v) = values else { return String::new() };
        let (t, r, b, l) = (&v[0], &v[1], &v[2], &v[3]);
        if l == r {
            if t == b {
                if t == r { t.clone() } else { format!("{t} {r}") }
            } else {
                format!("{t} {r} {b}")
            }
        } else {
            format!("{t} {r} {b} {l}")
        }
    }

    /// `CSSContainerRule.containerName` / `containerQuery`.
    pub fn container_parts(&self, path: &[u32]) -> (String, String) {
        match self.rule_at(path).map(|r| &r.kind) {
            Some(RuleKind::Container { name, query }) => (name.clone().unwrap_or_default(), query.clone()),
            _ => (String::new(), String::new()),
        }
    }

    /// `CSSScopeRule.start` / `end`: `None` is `null`.
    pub fn scope_parts(&self, path: &[u32]) -> (Option<String>, Option<String>) {
        match self.rule_at(path).map(|r| &r.kind) {
            Some(RuleKind::Scope { start, end }) => (start.clone(), end.clone()),
            _ => (None, None),
        }
    }

    /// `CSSPropertyRule`: `(name, syntax, inherits, initialValue)`. `syntax` is the string's contents
    /// (`<length>`), `inherits` is `true` only for the keyword `true`, and an absent `initial-value` is
    /// `None` (`null`).
    pub fn property_parts(&self, path: &[u32]) -> (String, String, bool, Option<String>) {
        match self.rule_at(path).map(|r| &r.kind) {
            Some(RuleKind::Property { name, block }) => {
                let raw = block.get("syntax");
                let mut pi = cssparser::ParserInput::new(raw);
                let mut p = cssparser::Parser::new(&mut pi);
                let syntax = p.expect_string().map(|s| s.to_string()).unwrap_or_default();
                let inherits = block.get("inherits").trim().eq_ignore_ascii_case("true");
                let initial = block.has("initial-value").then(|| block.get("initial-value").to_string());
                (name.clone(), syntax, inherits, initial)
            }
            _ => (String::new(), String::new(), false, None),
        }
    }

    /// `CSSPageRule.selectorText`.
    pub fn page_selector(&self, path: &[u32]) -> String {
        match self.rule_at(path).map(|r| &r.kind) {
            Some(RuleKind::Page { selector, .. }) => selector.clone(),
            _ => String::new(),
        }
    }

    /// `CSSPageRule.selectorText`'s setter: a `<page-selector>` -- an optional name and pseudo-pages
    /// (`:left`, `:right`, `:first`, `:blank`) -- replaces the old one; anything else leaves it, as
    /// Blink's `CSSPageRule::setSelectorText` does (Chrome 150: `x` and `:first` stick, `bogus!` does
    /// not, and `cssText` reads `@page x { ... }`).
    pub fn set_page_selector(&mut self, path: &[u32], text: &str) -> bool {
        let Some(sel) = parse_page_selector(text) else { return false };
        match self.rule_at_mut(path).map(|r| &mut r.kind) {
            Some(RuleKind::Page { selector, .. }) => {
                *selector = sel;
                true
            }
            _ => false,
        }
    }

    /// Write one descriptor. `false` means the value did not fit the descriptor's grammar, which
    /// the CSSOM drops silently — the old value stands.
    pub fn set_descriptor(&mut self, path: &[u32], name: &str, value: &str) -> bool {
        let Some(rule) = self.rule_at_mut(path) else { return false };
        let changed = match &mut rule.kind {
            RuleKind::PositionTry { block, .. } => at_rules::position_try_set(block, name, value),
            RuleKind::CounterStyle { block, .. } => at_rules::counter_style_set(block, name, value),
            RuleKind::FontFace { block } | RuleKind::Page { block, .. } => {
                if value.is_empty() {
                    block.remove(name);
                } else {
                    block.set(name, value.to_string());
                }
                true
            }
            _ => false,
        };
        if changed {
            self.rebuild();
        }
        changed
    }

    /// `cssRules[i].style.cssText` — the declaration block without its braces, for whichever kind
    /// of rule it is.
    pub fn rule_block_text(&self, path: &[u32]) -> String {
        match self.rule_at(path) {
            Some(rule) => match rule.kind.block() {
                Some(block) => block.css_text(),
                None if matches!(rule.kind, RuleKind::Keyframe { .. }) => self.declaration_block_text(rule.decls),
                None => self.style_block_text(rule),
            },
            None => String::new(),
        }
    }

    /// `conditionText` of a `@media` or `@supports` rule.
    pub fn condition_text(&self, path: &[u32]) -> String {
        match self.rule_at(path).map(|r| &r.kind) {
            Some(RuleKind::Media { text, .. }) => text.clone(),
            Some(RuleKind::Supports { condition, .. }) => condition.clone(),
            Some(RuleKind::Container { name, query }) => match name {
                Some(n) => format!("{n} {query}"),
                None => query.clone(),
            },
            _ => String::new(),
        }
    }

    /// `CSSLayerStatementRule.nameList`.
    pub fn layer_name_list(&self, path: &[u32]) -> Vec<String> {
        match self.rule_at(path).map(|r| &r.kind) {
            Some(RuleKind::LayerStatement { names }) => names.clone(),
            _ => Vec::new(),
        }
    }

    /// `CSSKeyframeRule.keyText`.
    pub fn key_text(&self, path: &[u32]) -> String {
        match self.rule_at(path).map(|r| &r.kind) {
            Some(RuleKind::Keyframe { offsets }) => key_text(offsets),
            _ => String::new(),
        }
    }

    /// `keyText`'s setter: a keyframe selector list, or nothing changes.
    pub fn set_key_text(&mut self, path: &[u32], text: &str) -> bool {
        let Some(offsets) = parse_key_text(text) else { return false };
        let Some(rule) = self.rule_at_mut(path) else { return false };
        let RuleKind::Keyframe { offsets: slot } = &mut rule.kind else { return false };
        *slot = offsets;
        self.rebuild();
        true
    }

    /// `CSSKeyframesRule.findRule(key)`: the index of the LAST keyframe whose selector is `key`.
    pub fn find_keyframe(&self, path: &[u32], key: &str) -> Option<usize> {
        let wanted = parse_key_text(key)?;
        self.rule_at(path)?
            .children
            .iter()
            .rposition(|k| matches!(&k.kind, RuleKind::Keyframe { offsets } if *offsets == wanted))
    }

    /// Where an insert into the list at `parent` parses its text: the sheet's own list, a grouping
    /// rule's, a style rule's (or anything inside one), or a `@keyframes`.
    fn insert_context(&self, parent: &[u32]) -> Option<InsertContext> {
        if parent.is_empty() {
            return Some(InsertContext::Sheet);
        }
        let mut nested = false;
        for n in 1..=parent.len() {
            match &self.rule_at(&parent[..n])?.kind {
                RuleKind::Style => nested = true,
                RuleKind::Keyframes { .. } if n == parent.len() => return Some(InsertContext::Keyframes),
                RuleKind::Media { .. }
                | RuleKind::Supports { .. }
                | RuleKind::LayerBlock { .. }
                | RuleKind::Container { .. }
                | RuleKind::Scope { .. }
                | RuleKind::StartingStyle => {}
                _ if n == parent.len() => return None,
                _ => {}
            }
        }
        Some(if nested { InsertContext::Nested } else { InsertContext::Group })
    }

    /// `insertRule` on the list at `parent` (the top level for an empty path). Returns the index
    /// the rule landed at, which is the index that was asked for.
    ///
    /// Text that is not exactly one rule is a `SyntaxError` rather than a silent no-op: `insertRule`
    /// is one of the few CSSOM entry points that is specified to throw, and CSS-in-JS libraries
    /// rely on it doing so to detect an unsupported rule.
    pub fn insert_rule_at(&mut self, parent: &[u32], text: &str, index: usize) -> Result<usize, CssomError> {
        let len = self.list_len(parent).ok_or(CssomError::Syntax)?;
        if index > len {
            return Err(CssomError::IndexSize);
        }
        let at = self.insert_context(parent).ok_or(CssomError::Syntax)?;
        let parsed = parse_rule_in(self.origin, text, at);
        if parsed.rules().len() != 1 {
            return Err(CssomError::Syntax);
        }
        let is_import = matches!(parsed.rules()[0].kind, RuleKind::Import(_));
        let is_statement = matches!(parsed.rules()[0].kind, RuleKind::Import(_) | RuleKind::LayerStatement { .. });
        if at == InsertContext::Sheet {
            // CSSOM "insert a CSS rule" step 5: an `@import` may not follow a rule that is not an
            // `@import` or a layer statement, and nothing else may precede an `@import`.
            let list = self.list_at(parent).unwrap_or(&[]);
            let before_ok = list[..index]
                .iter()
                .all(|r| matches!(r.kind, RuleKind::Import(_) | RuleKind::LayerStatement { .. }));
            let after_has_import = list[index..].iter().any(|r| matches!(r.kind, RuleKind::Import(_)));
            if (is_import && !before_ok) || (!is_statement && after_has_import) {
                return Err(CssomError::Hierarchy);
            }
        } else if is_import {
            return Err(CssomError::Hierarchy);
        }
        let mut rules = self.absorb(parsed);
        let rule = rules.remove(0);
        self.list_mut(parent).ok_or(CssomError::Syntax)?.insert(index, rule);
        self.rebuild();
        Ok(index)
    }

    /// `insertRule` on the sheet's own list.
    pub fn insert_rule(&mut self, text: &str, index: usize) -> Result<usize, CssomError> {
        self.insert_rule_at(&[], text, index)
    }

    /// `deleteRule` on the list at `parent`.
    pub fn delete_rule_at(&mut self, parent: &[u32], index: usize) -> Result<(), CssomError> {
        let list = self.list_mut(parent).ok_or(CssomError::IndexSize)?;
        if index >= list.len() {
            return Err(CssomError::IndexSize);
        }
        list.remove(index);
        self.rebuild();
        Ok(())
    }

    /// `deleteRule` on the sheet's own list.
    pub fn delete_rule(&mut self, index: usize) -> Result<(), CssomError> {
        self.delete_rule_at(&[], index)
    }

    /// `replaceSync`: throw away everything and parse afresh.
    ///
    /// The old text pool goes with it, which is the only place in the CSSOM where the pool shrinks
    /// — `insertRule` only ever appends, so a page that inserts and deletes in a loop grows the
    /// pool without bound. That is a known and bounded leak (the text of deleted rules), not a
    /// correctness problem, and it is the price of spans being stable.
    /// The per-sheet flags survive: replacing a sheet's RULES does not re-enable a disabled sheet,
    /// it does not make a cross-origin one readable, and it does not move the sheet's base URL.
    pub fn replace_sync(&mut self, text: &str) {
        let fresh = parse_stylesheet(self.origin, text);
        let disabled = self.disabled;
        let origin_clean = self.origin_clean;
        let base_url = self.base_url.take();
        let own_media = self.own_media.take();
        *self = fresh;
        self.own_media = own_media;
        self.disabled = disabled;
        self.origin_clean = origin_clean;
        self.base_url = base_url;
    }

    /// `selectorText`'s setter. An invalid selector leaves the rule alone, which is what the spec
    /// says and, more to the point, what a page that feature-detects by assigning garbage expects.
    pub fn set_selector_text(&mut self, path: &[u32], text: &str) -> bool {
        let nested = path.len() > 1
            && (1..path.len()).any(|n| matches!(self.rule_at(&path[..n]).map(|r| &r.kind), Some(RuleKind::Style)));
        let parsed = if nested { Selector::parse_nested(text) } else { Selector::parse_list(text) };
        let Ok(selectors) = parsed else { return false };
        match self.rule_at_mut(path) {
            // An at-rule has no selector list to set. `@position-try` does not even expose the
            // member, so reaching here means a page found the rule through `CSSStyleRule`'s
            // accessor -- which leaves it alone in a browser too.
            Some(rule) if rule.kind == RuleKind::Style => {
                rule.selectors = selectors;
                self.rebuild();
                true
            }
            _ => false,
        }
    }

    /// A style rule's own declarations replaced by those of `text`, the rule itself -- its selectors and the rules
    /// nested in it -- kept where it is: what a write through `rule.style` changes in Blink, whose `CSSStyleRule::style`
    /// edits the rule's property set and nothing else. Rebuilding the rule from `selector { text }` (the way
    /// `insertRule` makes one) dropped every nested rule: Chrome 152 keeps `.a { color: red; & .b { color: blue } }`'s
    /// `& .b` -- the same `CSSRule` object, still applying -- through `.a`'s `style.setProperty('width', '1px')`
    /// (`c3rule.html`, `vrule.html`, 2026-10-23). `false` when `path` is not a style rule or the declarations do not
    /// parse as its block.
    pub fn set_style_block(&mut self, path: &[u32], text: &str) -> bool {
        let Some((_, parent)) = path.split_last() else { return false };
        let selector = match self.rule_at(path) {
            Some(rule) if rule.kind == RuleKind::Style => rule.selector_text(),
            _ => return false,
        };
        let Some(at) = self.insert_context(parent) else { return false };
        // the declarations parsed as a rule's in the rule's own context; only its block is taken
        let parsed = parse_rule_in(self.origin, &format!("{selector} {{ {text} }}"), at);
        if parsed.rules().len() != 1 || parsed.rules()[0].kind != RuleKind::Style {
            return false;
        }
        let fresh = self.absorb(parsed).remove(0);
        let Some(rule) = self.rule_at_mut(path) else { return false };
        rule.decls = fresh.decls;
        rule.custom = fresh.custom;
        self.rebuild();
        true
    }
}

/// One `<page-selector>`: `<ident>? <pseudo-page>*`, at least one of the two, no whitespace inside;
/// the serialization is the text with the pseudo-page keywords lower-cased.
fn parse_page_selector(text: &str) -> Option<String> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    let (name, rest) = match t.find(':') {
        Some(i) => (&t[..i], &t[i..]),
        None => (t, ""),
    };
    let ident_ok = |s: &str| {
        let mut cs = s.chars();
        match cs.next() {
            Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '-' || !c.is_ascii() => {}
            _ => return false,
        }
        s != "-" && cs.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || !c.is_ascii())
    };
    if !name.is_empty() && !ident_ok(name) {
        return None;
    }
    let mut out = name.to_string();
    for p in rest.split(':').skip(1) {
        let k = p.to_ascii_lowercase();
        if !matches!(k.as_str(), "left" | "right" | "first" | "blank") {
            return None;
        }
        out.push(':');
        out.push_str(&k);
    }
    Some(out)
}

/// `0%, 50%` from offsets.
fn key_text(offsets: &[f32]) -> String {
    offsets.iter().map(|o| percent(*o)).collect::<Vec<_>>().join(", ")
}

/// A keyframe selector list as offsets, or `None` when it is not one.
fn parse_key_text(text: &str) -> Option<Vec<f32>> {
    let mut out = Vec::new();
    for piece in text.split(',') {
        let piece = piece.trim();
        out.push(if piece.eq_ignore_ascii_case("from") {
            0.0
        } else if piece.eq_ignore_ascii_case("to") {
            1.0
        } else {
            let v = piece.strip_suffix('%')?.trim().parse::<f32>().ok()?;
            if !(0.0..=100.0).contains(&v) {
                return None;
            }
            v / 100.0
        });
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::rules::Origin;

    fn sheet(text: &str) -> Stylesheet {
        parse_stylesheet(Origin::Author, text)
    }
    fn texts(s: &Stylesheet) -> Vec<String> {
        s.rules().iter().map(|r| s.rule_css_text(r)).collect()
    }

    // The exact spacing is the point: a page reads this back and compares it.
    #[test]
    fn a_rule_serializes_the_way_chrome_writes_it() {
        assert_eq!(texts(&sheet("p{color:red}")), vec!["p { color: red; }"]);
        assert_eq!(
            texts(&sheet("p { color: red; width: 1px }")),
            vec!["p { color: red; width: 1px; }"],
            "one space between declarations, and a semicolon after the last"
        );
        assert_eq!(texts(&sheet("p { }")), vec!["p { }"]);
        assert_eq!(texts(&sheet("H1 , .lede{color:red}")), vec!["H1, .lede { color: red; }"]);
    }

    // The whole reason `@position-try` is parsed at all: a sheet whose only rule is one of these
    // used to have NO rules, so `sheet.cssRules[0]` was `undefined` and every read off it threw.
    #[test]
    fn a_descriptor_at_rule_is_a_rule_the_cssom_can_see() {
        let s = sheet("@position-try --p { top: 0px }");
        assert_eq!(s.rules().len(), 1);
        assert_eq!(s.rule_kind_code(&[0]), RULE_POSITION_TRY);
        assert_eq!(s.at_rule_name(&[0]), "--p");
        assert_eq!(s.descriptor(&[0], "top"), "0px");
        assert_eq!(s.descriptor(&[0], "margin"), "", "a descriptor the rule does not set is empty");
        assert_eq!(texts(&s), vec!["@position-try --p { top: 0px; }"]);
    }

    #[test]
    fn a_counter_style_rule_keeps_its_descriptors_serialized() {
        let s = sheet("@counter-style c { system: cyclic; symbols: x; suffix: '.' }");
        assert_eq!(s.rule_kind_code(&[0]), RULE_COUNTER_STYLE);
        assert_eq!(s.at_rule_name(&[0]), "c");
        assert_eq!(s.descriptor(&[0], "system"), "cyclic");
        assert_eq!(s.descriptor(&[0], "symbols"), "x");
        assert_eq!(s.descriptor(&[0], "suffix"), "\".\"", "a CSS string reads back double-quoted");
    }

    // An at-rule sits in `cssRules` where it was written, not at the end and not nowhere.
    #[test]
    fn at_rules_keep_their_place_among_the_style_rules() {
        let s = sheet("a { color: red } @position-try --p { top: 0px } b { color: blue }");
        assert_eq!(s.rules().len(), 3);
        assert_eq!(s.rule_kind_code(&[0]), RULE_STYLE);
        assert_eq!(s.rule_kind_code(&[1]), RULE_POSITION_TRY);
        assert_eq!(s.rule_kind_code(&[2]), RULE_STYLE);
    }

    // A prelude that is not a name at all leaves NO rule -- the whole at-rule is dropped, the way
    // cssparser's recovery already drops `@supports`.
    #[test]
    fn a_descriptor_at_rule_without_a_valid_name_is_not_a_rule() {
        assert_eq!(sheet("@position-try p { top: 0px } b { color: blue }").rules().len(), 1);
        assert_eq!(sheet("@counter-style none { system: cyclic } b { color: blue }").rules().len(), 1);
    }

    #[test]
    fn only_a_counter_style_rule_can_be_renamed() {
        let mut s = sheet("@counter-style c { system: cyclic }");
        assert!(s.set_at_rule_name(&[0], "x"));
        assert_eq!(s.at_rule_name(&[0]), "x");
        assert!(!s.set_at_rule_name(&[0], "none"), "`none` is not a counter-style-name");
        assert_eq!(s.at_rule_name(&[0]), "x");

        let mut s = sheet("@position-try --p { top: 0px }");
        assert!(!s.set_at_rule_name(&[0], "--q"), "CSSPositionTryRule.name is readonly");
        assert_eq!(s.at_rule_name(&[0]), "--p");
    }

    #[test]
    fn a_descriptor_assignment_the_grammar_rejects_leaves_the_value_alone() {
        let mut s = sheet("@position-try --p { top: 0px }");
        assert!(!s.set_descriptor(&[0], "top", "x"));
        assert_eq!(s.descriptor(&[0], "top"), "0px");
        assert!(s.set_descriptor(&[0], "left", "2px"));
        assert_eq!(s.descriptor(&[0], "left"), "2px");
    }

    #[test]
    fn important_survives_the_round_trip() {
        assert_eq!(texts(&sheet("p{color:red!important}")), vec!["p { color: red !important; }"]);
    }

    #[test]
    fn a_shorthand_serializes_as_the_shorthand_chrome_writes() {
        assert_eq!(
            texts(&sheet("p { margin: 0 auto }")),
            vec!["p { margin: 0px auto; }"],
            "Chrome re-groups the longhands into `margin`, its unitless zero a length (2026-10-08)"
        );
    }

    // An empty block is the empty string, not a space: a page checking `cssText === ""` is asking
    // whether anything is set at all.
    #[test]
    fn an_empty_declaration_block_serializes_to_nothing() {
        let s = sheet("p { }");
        assert_eq!(s.declaration_block_text(s.rules()[0].decls), "");
    }

    #[test]
    fn insert_rule_puts_it_where_it_was_asked_to() {
        let mut s = sheet("a { color: red } c { color: teal }");
        assert_eq!(s.insert_rule("b { color: blue }", 1), Ok(1));
        assert_eq!(
            texts(&s),
            vec!["a { color: red; }", "b { color: blue; }", "c { color: teal; }"]
        );
        assert_eq!(s.insert_rule("d { color: pink }", 3), Ok(3), "the end is a valid index");
        assert_eq!(s.rules().len(), 4);
    }

    // The inserted rule's declarations point into text the sheet did not have a moment ago, so this
    // is the case where the span bookkeeping is either right or silently reading someone else's
    // bytes.
    #[test]
    fn an_inserted_rules_values_survive_being_moved_into_the_sheets_pool() {
        let mut s = sheet("a { color: red }");
        s.insert_rule("b { width: 12px; height: 34px }", 1).unwrap();
        assert_eq!(
            texts(&s),
            vec!["a { color: red; }", "b { width: 12px; height: 34px; }"]
        );
        // And again, so the second insert's offsets are computed against an already-grown pool.
        s.insert_rule("c { color: rebeccapurple }", 2).unwrap();
        assert_eq!(texts(&s)[2], "c { color: rebeccapurple; }");
        assert_eq!(texts(&s)[0], "a { color: red; }", "the first rule is undisturbed");
    }

    #[test]
    fn an_inserted_media_rule_keeps_pointing_at_its_own_condition() {
        let mut s = sheet("@media (min-width: 600px) { a { color: red } }");
        s.insert_rule("@media print { b { color: blue } }", 1).unwrap();
        assert_eq!(s.rules().len(), 2);
        let wide = crate::style::media::MediaContext::default();
        assert!(s.rule_applies(&s.cascade_rules()[0], &wide));
        assert!(!s.rule_applies(&s.cascade_rules()[1], &wide), "the print rule is not live on screen");
    }

    #[test]
    fn insert_rule_reports_the_two_ways_it_can_fail() {
        let mut s = sheet("a { color: red }");
        assert_eq!(s.insert_rule("b { color: blue }", 9), Err(CssomError::IndexSize));
        assert_eq!(s.insert_rule("not a rule", 0), Err(CssomError::Syntax));
        assert_eq!(s.insert_rule("a {} b {}", 0), Err(CssomError::Syntax), "exactly one rule");
        assert_eq!(s.rules().len(), 1, "and neither of them changed anything");
    }

    #[test]
    fn delete_rule_removes_one_and_renumbers_the_rest() {
        let mut s = sheet("a { color: red } b { color: blue } c { color: teal }");
        assert_eq!(s.delete_rule(1), Ok(()));
        assert_eq!(texts(&s), vec!["a { color: red; }", "c { color: teal; }"]);
        assert_eq!(s.delete_rule(2), Err(CssomError::IndexSize));
    }

    #[test]
    fn replace_sync_starts_over() {
        let mut s = sheet("a { color: red } b { color: blue }");
        s.replace_sync("z { color: pink }");
        assert_eq!(texts(&s), vec!["z { color: pink; }"]);
        s.replace_sync("");
        assert!(s.rules().is_empty());
    }

    #[test]
    fn replace_sync_keeps_the_sheet_disabled_if_it_was() {
        let mut s = sheet("a { color: red }");
        s.disabled = true;
        s.replace_sync("b { color: blue }");
        assert!(s.disabled, "replacing a sheet's rules does not re-enable the sheet");
    }

    #[test]
    fn setting_selector_text_replaces_the_whole_list() {
        let mut s = sheet("a { color: red }");
        assert!(s.set_selector_text(&[0], "b, .c"));
        assert_eq!(texts(&s), vec!["b, .c { color: red; }"]);
    }

    // A page that feature-detects by assigning garbage must see the old value, not an exception and
    // not an empty selector.
    #[test]
    fn setting_an_invalid_selector_leaves_the_rule_alone() {
        let mut s = sheet("a { color: red }");
        for bad in ["", " ", "!!", "123", "a >"] {
            assert!(!s.set_selector_text(&[0], bad), "{bad:?} should be rejected");
            assert_eq!(texts(&s), vec!["a { color: red; }"]);
        }
        assert!(!s.set_selector_text(&[9], "b"), "and so is an index that is not there");
    }

    // Chrome 152 (`c3rule.html`, `vrule.html`, 2026-10-23): a write through a style rule's `style` keeps its nested
    // rules -- `.a { color: red; & .b { ... } }` with `width: 1px` set reads the nested rule back after it, still one of
    // `cssRules` -- and touches no rule that is not a style rule.
    #[test]
    fn a_style_rules_block_is_replaced_and_its_nested_rules_kept() {
        let mut s = sheet(".a { color: red; & .b { color: rgb(0, 0, 255) } } @font-face { font-family: f; src: local(A) }");
        assert!(s.set_style_block(&[0], "color: red; width: 1px;"));
        assert_eq!(s.css_text_at(&[0]), ".a {\n  color: red; width: 1px;\n  & .b { color: rgb(0, 0, 255); }\n}");
        assert_eq!(s.list_len(&[0]), Some(1));
        assert!(s.set_style_block(&[0, 0], "color: green;"), "a nested style rule is a style rule");
        assert_eq!(s.css_text_at(&[0, 0]), "& .b { color: green; }");
        assert!(!s.set_style_block(&[1], "font-family: g;"), "not a style rule");
        assert_eq!(s.css_text_at(&[1]), "@font-face { font-family: f; src: local(\"A\"); }");
        assert!(!s.set_style_block(&[7], "color: red;"));
    }

    // ── grouping rules -- every string below was read off Chrome 150's cssText ─────

    #[test]
    fn grouping_rules_serialize_their_children_on_indented_lines() {
        let s = sheet("@layer a, b.c; @layer { .anon { color: red } } @media screen and (min-width: 1px) { .m { color: red } @supports (color: red) { .ms { color: red } } }");
        assert_eq!(texts(&s), vec![
            "@layer a, b.c;".to_string(),
            "@layer {\n  .anon { color: red; }\n}".to_string(),
            "@media screen and (min-width: 1px) {\n  .m { color: red; }\n  @supports (color: red) {\n  .ms { color: red; }\n}\n}".to_string(),
        ]);
        assert_eq!(s.rule_kind_code(&[0]), RULE_LAYER_STATEMENT);
        assert_eq!(s.rule_kind_code(&[1]), RULE_LAYER_BLOCK);
        assert_eq!(s.rule_kind_code(&[2, 1]), RULE_SUPPORTS);
        assert_eq!(s.condition_text(&[2]), "screen and (min-width: 1px)");
        assert_eq!(s.at_rule_name(&[1]), "", "an anonymous layer's name is empty");
    }

    #[test]
    fn keyframes_and_nested_style_rules_serialize_like_chrome() {
        let s = sheet("@keyframes kf { from { opacity: 0 } 50% { opacity: .5 } to { opacity: 1 } } .p { color: red; & .c { color: blue } color: teal }");
        assert_eq!(texts(&s)[0], "@keyframes kf { \n  0% { opacity: 0; }\n  50% { opacity: 0.5; }\n  100% { opacity: 1; }\n}");
        assert_eq!(texts(&s)[1], ".p {\n  color: red;\n  & .c { color: blue; }\n  color: teal;\n}");
        assert_eq!(s.key_text(&[0, 1]), "50%");
        assert_eq!(s.find_keyframe(&[0], "to"), Some(2));
        assert_eq!(s.rule_kind_code(&[1, 1]), RULE_NESTED_DECLARATIONS);
    }

    #[test]
    fn insert_rule_takes_the_at_rules_chrome_takes() {
        let mut s = sheet("");
        for text in [
            "@keyframes x { from { opacity: 0 } to { opacity: 1 } }",
            "@layer foo { .a { color: red } }",
            "@layer foo, bar;",
            "@supports (display: grid) { .a { color: red } }",
            ".a { & .b { color: red } }",
            "@media print { .a{} }",
            "@font-face { font-family: Y; src: url(y.woff2) }",
            "& .b { color: red }",
        ] {
            assert_eq!(s.insert_rule(text, 0), Ok(0), "{text}");
        }
        assert_eq!(s.rules().len(), 8);
        assert_eq!(s.insert_rule("@import url(x.css);", 1), Err(CssomError::Hierarchy), "not after a style rule");
    }

    #[test]
    fn a_grouping_rule_takes_inserts_and_a_style_rule_takes_nested_ones() {
        let mut s = sheet("@media screen { .a { color: red } } .b { color: red }");
        assert_eq!(s.insert_rule_at(&[0], ".z { color: blue }", 1), Ok(1));
        assert_eq!(s.list_len(&[0]), Some(2));
        assert_eq!(s.insert_rule_at(&[1], "& .c { color: blue }", 0), Ok(0));
        assert_eq!(texts(&s)[1], ".b {\n  color: red;\n  & .c { color: blue; }\n}");
        assert_eq!(s.cascade_rules().len(), 4, ".a, .z, .b and the nested .c all cascade");
        assert_eq!(s.delete_rule_at(&[0], 0), Ok(()));
        assert_eq!(s.cascade_rules().len(), 3);
    }
}
