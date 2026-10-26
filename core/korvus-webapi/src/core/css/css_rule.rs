//! `core/css/css_rule.rs` — `CSSRule` and the rule interfaces this engine's parser can produce, as
//! `interface!` tables (the grouping rules).
//!
//! # What this replaces, and the instance tell it removes
//!
//! The kernel's `BOOTSTRAP` string literal built every rule as a plain object with FOUR TO SIX OWN
//! ACCESSORS stamped on by `Object.defineProperty` — `cssText`, `selectorText`, `style`, `type`,
//! `parentStyleSheet`, `parentRule` — one set per rule object, minted afresh by `mkRule`/`mkAtRule`.
//! A browser's rule owns NOTHING: every one of those lives on `CSSRule.prototype` or
//! `CSSStyleRule.prototype`, and `Object.getOwnPropertyNames(sheet.cssRules[0])` is empty.
//!
//! # How a rule knows which rule it is
//!
//! V8 privates: `RULE_SHEET` (the `CSSStyleSheet` it belongs to) and `RULE_PATH` — the rule's PATH
//! from its handle's top-level list, `"2"` for `cssRules[2]` and `"2.0"` for `cssRules[2].cssRules[0]`
//! (`@media`, `@supports`, `@layer`, `@import`, `@keyframes` and nesting style rules hold
//! rules, so an index is not an address). A private is invisible to `Object.getOwnPropertyNames`,
//! `Reflect.ownKeys` and `JSON.stringify`, so the instance still owns nothing, and it is also the
//! BRAND: a row whose receiver has no `RULE_SHEET` throws `Illegal invocation`, which is what Chrome
//! answers for `CSSStyleRule.prototype.selectorText` read off a foreign object.
//!
//! # Which interface, measured
//!
//! Chrome 150 over a `<style>` holding one of each (`t4.html` in the unit's scratch):
//!
//! ```text
//! @layer a, b.c;            CSSLayerStatementRule  type 0    @keyframes kf {…}   CSSKeyframesRule type 7
//! @layer {…} / @layer a {…} CSSLayerBlockRule      type 0      from {…}          CSSKeyframeRule  type 8
//! @media … {…}              CSSMediaRule           type 4    @font-face {…}      CSSFontFaceRule  type 5
//! @supports … {…}           CSSSupportsRule        type 12   @import url(x);     CSSImportRule    type 3
//! .p { & .c {…} color: … }  CSSStyleRule (type 1) whose cssRules hold CSSStyleRule, CSSNestedDeclarations (type 0)
//! ```

use deno_core::v8;

use crate::binding::protos::{shell_proto, str_val, throw_dom, throw_type_error};
use crate::core::css::css_style_declaration::priv_get;
use crate::core::css::cssom;
use crate::core::css::style_sheet::{sheet_handle, SHEET_ID};
use crate::core::dom::element::priv_set;
use crate::install::MemberDef;
use page::style::cssom as codes;

/// The `CSSStyleSheet` a rule (or a rule's declaration block) belongs to.
pub(crate) const RULE_SHEET: &str = "korvus_rule_sheet";
/// Its path from the handle's top-level list, as `"i.j.k"`.
pub(crate) const RULE_PATH: &str = "korvus_rule_path";
/// The `[SameObject]` declaration block of a rule (`rule.style === rule.style`).
const RULE_STYLE: &str = "korvus_rule_style";
/// The rule this one is nested inside, when it is.
const RULE_PARENT: &str = "korvus_rule_parent";
/// The child rule WRAPPERS and the `CSSRuleList` over them, as a sheet keeps its own.
const RULE_CHILDREN: &str = "korvus_rule_children";
const RULE_CHILDLIST: &str = "korvus_rule_childlist";
/// `CSSImportRule.styleSheet`, minted once.
const RULE_IMPORTED: &str = "korvus_rule_imported";
/// `CSSMediaRule.media` / `CSSImportRule.media`, minted once.
const RULE_MEDIA: &str = "korvus_rule_media";

// ── the brand, and the (sheet, path) a receiver stands for ───────────────────────────────────────

fn path_string(path: &[u32]) -> String {
    path.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(".")
}

fn parse_path(text: &str) -> Option<Vec<u32>> {
    if text.is_empty() {
        return Some(Vec::new());
    }
    text.split('.').map(|p| p.parse().ok()).collect()
}

/// The `(sheet object, path)` `this` is, or `None` with `Illegal invocation` thrown.
fn rule_recv<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    this: v8::Local<v8::Object>,
) -> Option<(v8::Local<'s, v8::Object>, Vec<u32>)> {
    match rule_pos(scope, this) {
        Some(p) => Some(p),
        None => {
            throw_type_error(scope, "Illegal invocation");
            None
        }
    }
}

/// The `(sheet object, path)` pair, without throwing — the same read the declaration-block rows in
/// `css_style_declaration.rs` make to tell a rule's block from an element's inline style.
pub(crate) fn rule_pos<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    o: v8::Local<v8::Object>,
) -> Option<(v8::Local<'s, v8::Object>, Vec<u32>)> {
    let sheet = priv_get(scope, o, RULE_SHEET).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    // A sheet that has lost its handle is not a sheet; check the slot rather than trusting the chain.
    priv_get(scope, sheet, SHEET_ID)?;
    let path = priv_get(scope, o, RULE_PATH).map(|v| v.to_rust_string_lossy(scope))?;
    Some((sheet, parse_path(&path)?))
}

fn handle_of(scope: &mut v8::PinScope, sheet: v8::Local<v8::Object>) -> u32 {
    sheet_handle(scope, sheet)
}

/// Run `f` against the page's `OpState` with this sheet's handle.
fn with_state<R>(scope: &mut v8::PinScope, sheet: v8::Local<v8::Object>, f: impl FnOnce(&mut deno_core::OpState, u32) -> R) -> R {
    let id = handle_of(scope, sheet);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    f(&mut st, id)
}

fn kind_of(scope: &mut v8::PinScope, sheet: v8::Local<v8::Object>, path: &[u32]) -> u32 {
    with_state(scope, sheet, |st, id| cssom::rule_kind(st, id, path))
}

/// A member name as its CSS property/descriptor name: `marginTop` -> `margin-top`. A name the capture
/// already spells dashed (`margin-top` IS one of the 70 `CSSPositionTryDescriptors` members) has no
/// uppercase letters and comes back unchanged — the same rule the deleted `cssName` helper applied.
fn css_name(member: &str) -> String {
    let mut out = String::with_capacity(member.len() + 4);
    for c in member.chars() {
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

// ── minting a rule wrapper ───────────────────────────────────────────────────────────────────────

/// The interface a rule kind code is.
fn iface_of(kind: u32) -> &'static str {
    match kind {
        codes::RULE_POSITION_TRY => "CSSPositionTryRule",
        codes::RULE_COUNTER_STYLE => "CSSCounterStyleRule",
        codes::RULE_MEDIA => "CSSMediaRule",
        codes::RULE_SUPPORTS => "CSSSupportsRule",
        codes::RULE_LAYER_BLOCK => "CSSLayerBlockRule",
        codes::RULE_LAYER_STATEMENT => "CSSLayerStatementRule",
        codes::RULE_IMPORT => "CSSImportRule",
        codes::RULE_KEYFRAMES => "CSSKeyframesRule",
        codes::RULE_KEYFRAME => "CSSKeyframeRule",
        codes::RULE_FONT_FACE => "CSSFontFaceRule",
        codes::RULE_NESTED_DECLARATIONS => "CSSNestedDeclarations",
        codes::RULE_CONTAINER => "CSSContainerRule",
        codes::RULE_SCOPE => "CSSScopeRule",
        codes::RULE_PROPERTY => "CSSPropertyRule",
        codes::RULE_PAGE => "CSSPageRule",
        codes::RULE_STARTING_STYLE => "CSSStartingStyleRule",
        _ => "CSSStyleRule",
    }
}

/// The wrapper for the rule at `path` of `sheet`, on the prototype of whichever interface the parser
/// says that rule is. It carries privates and NOTHING ELSE — no own accessors, no backing field a
/// page could enumerate.
pub(crate) fn make_rule<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    sheet: v8::Local<'s, v8::Object>,
    path: &[u32],
    parent: Option<v8::Local<'s, v8::Object>>,
) -> Option<v8::Local<'s, v8::Object>> {
    let iface = iface_of(kind_of(scope, sheet, path));
    let proto = shell_proto(scope, iface)?;
    let r = crate::binding::protos::mint_on(scope, proto.into());
    priv_set(scope, r, RULE_SHEET, sheet.into());
    let pv = str_val(scope, &path_string(path));
    priv_set(scope, r, RULE_PATH, pv);
    if let Some(p) = parent {
        priv_set(scope, r, RULE_PARENT, p.into());
    }
    Some(r)
}

/// The `[SameObject]` declaration block of a rule: a `CSSStyleDeclaration` (for a style rule, a
/// keyframe, nested declarations or a font face) or a `CSSPositionTryDescriptors` (for
/// `@position-try`), carrying the same privates so every member of `css_style_declaration.rs` and of
/// this file can find the rule behind it.
fn block_of<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    rule: v8::Local<'s, v8::Object>,
    sheet: v8::Local<'s, v8::Object>,
    path: &[u32],
    iface: &str,
) -> Option<v8::Local<'s, v8::Object>> {
    if let Some(b) = priv_get(scope, rule, RULE_STYLE).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        return Some(b);
    }
    let proto = shell_proto(scope, iface)?;
    let b = if iface == "CSSStyleDeclaration" {
        crate::core::css::css_style_declaration::new_rule_block(scope, proto)
    } else {
        let b = crate::binding::protos::mint_on(scope, proto.into());
        b
    };
    priv_set(scope, b, RULE_SHEET, sheet.into());
    let pv = str_val(scope, &path_string(path));
    priv_set(scope, b, RULE_PATH, pv);
    priv_set(scope, rule, RULE_STYLE, b.into());
    Some(b)
}

/// The child rule wrappers of a grouping rule, cached so `r.cssRules[0] === r.cssRules[0]` and
/// rebuilt when the count moved.
fn children_of<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    rule: v8::Local<'s, v8::Object>,
    sheet: v8::Local<'s, v8::Object>,
    path: &[u32],
) -> v8::Local<'s, v8::Array> {
    let n = with_state(scope, sheet, |st, id| cssom::list_len(st, id, path));
    if let Some(a) = priv_get(scope, rule, RULE_CHILDREN).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
        if a.length() == n {
            return a;
        }
    }
    let a = v8::Array::new(scope, n as i32);
    for i in 0..n {
        let mut child = path.to_vec();
        child.push(i);
        if let Some(r) = make_rule(scope, sheet, &child, Some(rule)) {
            a.set_index(scope, i, r.into());
        }
    }
    priv_set(scope, rule, RULE_CHILDREN, a.into());
    a
}

fn invalidate_children(scope: &mut v8::PinScope, rule: v8::Local<v8::Object>) {
    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
    priv_set(scope, rule, RULE_CHILDREN, u);
}

// ── reading and writing a declaration block ──────────────────────────────────────────────────────

/// The text of the declaration block of the rule at `path`, braces excluded.
pub(crate) fn block_text(scope: &mut v8::PinScope, sheet: v8::Local<v8::Object>, path: &[u32]) -> String {
    with_state(scope, sheet, |st, id| cssom::rule_style_text(st, id, path))
}

/// Replace a rule's whole declaration block with `text` — what `[PutForwards=cssText]` does for
/// `rule.style = "…"` and what `rule.style.cssText = "…"` does directly.
///
/// TWO KINDS OF RULE, TWO MECHANISMS, and the difference is the pipeline's, not a choice made here.
///
/// * A DESCRIPTOR AT-RULE (`@position-try`, `@counter-style`, `@font-face`) owns its block, so
///   `cssom::set_descriptor` can write it one declaration at a time. REPLACE, not merge: the recording
///   says `(o.style.top = "7px", o.style = "left: 1px", o.style.top)` is `""`, so every descriptor the
///   interface declares is cleared first and then the incoming text is parsed.
///
/// * A rule whose declarations live in the SHEET's pools (a style rule, a keyframe, nested
///   declarations) is REPARSED: the new rule text is inserted next to the old one and the old one
///   deleted, which leaves the same count and the same index, so no wrapper the page is holding goes
///   stale. If the new text does not parse as one rule the insert fails and NOTHING is changed, which
///   is the CSSOM's own answer for an unparsable block.
pub(crate) fn set_block_text(scope: &mut v8::PinScope, sheet: v8::Local<v8::Object>, path: &[u32], text: &str) {
    let id = handle_of(scope, sheet);
    if id == u32::MAX {
        return;
    }
    let Some((&index, parent)) = path.split_last() else { return };
    let kind = kind_of(scope, sheet, path);
    let rebuilt = |head: String| if text.trim().is_empty() { format!("{head} {{ }}") } else { format!("{head} {{ {text} }}") };
    let replacement = match kind {
        codes::RULE_STYLE => Some(rebuilt(with_state(scope, sheet, |st, id| cssom::rule_selector(st, id, path)))),
        codes::RULE_KEYFRAME => Some(rebuilt(with_state(scope, sheet, |st, id| cssom::key_text(st, id, path)))),
        codes::RULE_NESTED_DECLARATIONS => Some(text.to_string()),
        _ => None,
    };
    if let Some(rule) = replacement {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        if cssom::insert_rule_at(&mut st, id, parent, &rule, index + 1) >= 0 {
            cssom::delete_rule_at(&mut st, id, parent, index);
        }
        return;
    }
    // A descriptor at-rule: clear every descriptor the interface declares, then parse the text.
    let names: &[&str] = match kind {
        codes::RULE_POSITION_TRY => POSITION_TRY_DESCRIPTOR_NAMES,
        codes::RULE_COUNTER_STYLE => COUNTER_STYLE_DESCRIPTOR_NAMES,
        codes::RULE_FONT_FACE => FONT_FACE_DESCRIPTOR_NAMES,
        codes::RULE_PAGE => PAGE_DESCRIPTOR_NAMES,
        _ => return,
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    for n in names {
        cssom::set_descriptor(&mut st, id, path, &css_name(n), "");
    }
    for decl in text.split(';') {
        let Some(colon) = decl.find(':') else { continue };
        let name = decl[..colon].trim().to_ascii_lowercase();
        let value = decl[colon + 1..].trim();
        if name.is_empty() || value.is_empty() {
            continue;
        }
        cssom::set_descriptor(&mut st, id, path, &name, value);
    }
}

// ── `CSSRule` bodies ─────────────────────────────────────────────────────────────────────────────

/// `type` — the LEGACY rule-type numbering, frozen years ago: the kinds it named keep their numbers
/// (style 1, import 3, media 4, font-face 5, keyframes 7, keyframe 8, supports 12), and every rule
/// type added since reports `0`. Measured on Chrome 150 (this file's header table).
fn cb_rule_type(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    let n = match kind_of(scope, sheet, &path) {
        codes::RULE_STYLE => 1,
        codes::RULE_IMPORT => 3,
        codes::RULE_MEDIA => 4,
        codes::RULE_FONT_FACE => 5,
        codes::RULE_KEYFRAMES => 7,
        codes::RULE_KEYFRAME => 8,
        codes::RULE_SUPPORTS => 12,
        // Chrome 150: `@page` 6 and `@counter-style` 11 keep their legacy numbers;
        // `@container`, `@scope`, `@property` and `@starting-style` are 0.
        codes::RULE_PAGE => 6,
        codes::RULE_COUNTER_STYLE => 11,
        _ => 0,
    };
    rv.set_uint32(n);
}

fn cb_rule_css_text_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    let t = with_state(scope, sheet, |st, id| cssom::rule_text(st, id, &path));
    let v = str_val(scope, &t);
    rv.set(v);
}
/// `cssText`'s setter. CSSOM, verbatim: "The `cssText` attribute … on setting, do nothing." Chrome
/// does nothing too. A no-op body here is the whole implementation, not a gap.
fn cb_rule_css_text_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let _ = rule_recv(scope, args.this());
}

/// `parentRule` — the rule this one is nested inside, or null at the top of a sheet.
fn cb_rule_parent_rule(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(_) = rule_recv(scope, args.this()) else { return };
    match priv_get(scope, args.this(), RULE_PARENT) {
        Some(p) if p.is_object() => rv.set(p),
        _ => rv.set_null(),
    }
}

fn cb_rule_parent_sheet(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some((sheet, _)) = rule_recv(scope, args.this()) else { return };
    rv.set(sheet.into());
}

// ── `CSSStyleRule` bodies ────────────────────────────────────────────────────────────────────────

fn cb_style_rule_selector_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    let s = with_state(scope, sheet, |st, id| cssom::rule_selector(st, id, &path));
    let v = str_val(scope, &s);
    rv.set(v);
}
/// Assigning an INVALID selector leaves the rule alone rather than throwing: that is what the spec
/// says, and what a page feature-detecting by assigning garbage expects.
fn cb_style_rule_selector_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    let text = args.get(0).to_rust_string_lossy(scope);
    with_state(scope, sheet, |st, id| cssom::rule_set_selector(st, id, &path, &text));
}

fn cb_style_rule_style_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    match block_of(scope, args.this(), sheet, &path, "CSSStyleDeclaration") {
        Some(b) => rv.set(b.into()),
        None => rv.set_undefined(),
    }
}
/// `[PutForwards=cssText]`: `rule.style = "color: red"` REPLACES the block, exactly as
/// `rule.style.cssText = "color: red"` does.
fn cb_style_rule_style_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    let text = args.get(0).to_rust_string_lossy(scope);
    set_block_text(scope, sheet, &path, &text);
}

// ── `CSSGroupingRule` (and a style rule's nested list) ───────────────────────────────────────────

/// `cssRules` of a grouping rule or a nesting style rule -- a `CSSRuleList`, `[SameObject]`.
fn cb_group_css_rules<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    let arr = children_of(scope, args.this(), sheet, &path);
    let existing = priv_get(scope, args.this(), RULE_CHILDLIST).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    let list = crate::core::dom::collections::static_list_reuse(scope, "CSSRuleList", arr, existing);
    priv_set(scope, args.this(), RULE_CHILDLIST, list);
    rv.set(list);
}

/// The interface name a grouping rule's messages use.
fn group_iface(scope: &mut v8::PinScope, sheet: v8::Local<v8::Object>, path: &[u32]) -> &'static str {
    match kind_of(scope, sheet, path) {
        codes::RULE_STYLE => "CSSStyleRule",
        _ => "CSSGroupingRule",
    }
}

/// `insertRule(rule, index = 0)` on a grouping rule's list. The messages are the sheet's, naming the
/// interface Chrome names.
fn cb_group_insert_rule(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    let iface = group_iface(scope, sheet, &path);
    let text = args.get(0).to_rust_string_lossy(scope);
    let n = with_state(scope, sheet, |st, id| cssom::list_len(st, id, &path));
    let i = if args.get(1).is_undefined() { 0 } else { args.get(1).uint32_value(scope).unwrap_or(0) };
    if i > n {
        let msg = format!("Failed to execute 'insertRule' on '{iface}': the index {i} must be less than or equal to the length of the rule list.");
        throw_dom(scope, &msg, "IndexSizeError");
        return;
    }
    let r = with_state(scope, sheet, |st, id| cssom::insert_rule_at(st, id, &path, &text, i));
    if r == cssom::ERR_HIERARCHY {
        // Blink's `CSSGroupingRule::insertRule` wording, with the at-keyword spliced in.
        let msg = format!("Failed to execute 'insertRule' on '{iface}': '{}' rules cannot be inserted inside a group rule.", "@".to_string() + "import");
        throw_dom(scope, &msg, "HierarchyRequestError");
        return;
    }
    if r < 0 {
        let msg = format!("Failed to execute 'insertRule' on '{iface}': the rule '{text}' is invalid and cannot be parsed.");
        throw_dom(scope, &msg, "SyntaxError");
        return;
    }
    invalidate_children(scope, args.this());
    rv.set_int32(r);
}

fn cb_group_delete_rule(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    let iface = group_iface(scope, sheet, &path);
    let i = args.get(0).uint32_value(scope).unwrap_or(0);
    let ok = with_state(scope, sheet, |st, id| cssom::delete_rule_at(st, id, &path, i));
    if !ok {
        let msg = format!("Failed to execute 'deleteRule' on '{iface}': the index {i} is greated than the length of the rule list.");
        throw_dom(scope, &msg, "IndexSizeError");
        return;
    }
    invalidate_children(scope, args.this());
}

// ── `CSSConditionRule`, `CSSMediaRule`, `CSSLayerBlockRule`, `CSSLayerStatementRule` ─────────────

fn cb_condition_text(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    let t = with_state(scope, sheet, |st, id| cssom::condition_text(st, id, &path));
    // A `@media` rule's condition is its media query list SERIALIZED, not the prelude as written:
    // Chrome 150 answers `(max-width: 10px)` for `@media(max-width:10px)`, and `not all` for a query
    // that does not parse (`tools/realweb/seed/mq`).
    let t = if kind_of(scope, sheet, &path) == codes::RULE_MEDIA { cssom::media_serialize(scope, &t) } else { t };
    let v = str_val(scope, &t);
    rv.set(v);
}

/// `CSSMediaRule.media` / `CSSImportRule.media` -- a `MediaList` over the rule's own query text.
fn cb_rule_media_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    if let Some(m) = priv_get(scope, args.this(), RULE_MEDIA).filter(|v| v.is_object()) {
        rv.set(m);
        return;
    }
    let text = match kind_of(scope, sheet, &path) {
        codes::RULE_IMPORT => with_state(scope, sheet, |st, id| cssom::import_info(st, id, &path)).map(|i| i.1).unwrap_or_default(),
        _ => {
            let t = with_state(scope, sheet, |st, id| cssom::condition_text(st, id, &path));
            cssom::media_serialize(scope, &t)
        }
    };
    if let Some(m) = crate::core::css::style_sheet::detached_media_list(scope, &text) {
        priv_set(scope, args.this(), RULE_MEDIA, m.into());
        rv.set(m.into());
    }
}
/// `[PutForwards=mediaText]` -- the rule's list is a snapshot here, so the assignment reaches the
/// `MediaList` object and not the cascade.
fn cb_rule_media_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(_) = rule_recv(scope, args.this()) else { return };
}

fn cb_at_rule_name_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    let n = with_state(scope, sheet, |st, id| cssom::at_rule_name(st, id, &path));
    let v = str_val(scope, &n);
    rv.set(v);
}
fn cb_at_rule_name_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    let name = args.get(0).to_rust_string_lossy(scope);
    // A name that does not fit the rule's grammar leaves the rule alone rather than throwing.
    with_state(scope, sheet, |st, id| cssom::at_rule_set_name(st, id, &path, &name));
}

// ── `CSSContainerRule`, `CSSScopeRule`, `CSSPropertyRule`, `CSSPageRule` ───────────
//
// Chrome 150 over the unit's `atrules.html`:
//
// ```text
// @container card (width < 200px) {…}   conditionText "card (width < 200px)"  containerName "card"
//                                        containerQuery "(width < 200px)"
// @scope (.sc2) to (.stop) {…}          start ".sc2"  end ".stop"   (@scope (.sc) {…}: end null)
// @property --x { syntax: "<length>"; inherits: false; initial-value: 7px }
//                                        name "--x" syntax "<length>" inherits false initialValue "7px"
// @property --any { syntax: "*"; inherits: false }                      initialValue null
// @page :first { margin: 1in }          selectorText ":first"  style.margin "1in"
// ```

fn cb_container_name(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    let (name, _) = with_state(scope, sheet, |st, id| cssom::container_parts(st, id, &path));
    let v = str_val(scope, &name);
    rv.set(v);
}
fn cb_container_query(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    let (_, query) = with_state(scope, sheet, |st, id| cssom::container_parts(st, id, &path));
    let v = str_val(scope, &query);
    rv.set(v);
}
/// `readonly attribute FrozenArray<CSSContainerCondition> conditions` (C2): Blink's `CSSContainerRule::conditions`
/// keeps ONE `FrozenArray` per rule, of `{name, query}` dictionaries -- the same array and the same objects on every
/// read (`install::frozen_array`; Chrome 150 measured). This parser keeps one condition per rule, so the list is that
/// one, its unnamed `name` the empty string. It was the D2 stub's `[]`.
fn cb_container_conditions(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let Some((sheet, path)) = rule_recv(scope, this) else { return };
    let (name, query) = with_state(scope, sheet, |st, id| cssom::container_parts(st, id, &path));
    let answer = crate::install::frozen_array::answer(scope, this, "CSSContainerRule", "conditions", |_| 0.0, |scope| {
        // a `CSSContainerCondition` as Blink's dictionary converts: its members in sorted order, own data properties
        let o = v8::Object::new(scope);
        for (k, text) in [("name", &name), ("query", &query)] {
            let key = v8::String::new(scope, k)?;
            let v = str_val(scope, text);
            o.create_data_property(scope, key.into(), v)?;
        }
        Some(vec![o.into()])
    });
    if let Some(a) = answer {
        rv.set(a);
    }
}
fn cb_scope_start(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    match with_state(scope, sheet, |st, id| cssom::scope_parts(st, id, &path)).0 {
        Some(s) => {
            let v = str_val(scope, &s);
            rv.set(v);
        }
        None => rv.set_null(),
    }
}
fn cb_scope_end(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    match with_state(scope, sheet, |st, id| cssom::scope_parts(st, id, &path)).1 {
        Some(s) => {
            let v = str_val(scope, &s);
            rv.set(v);
        }
        None => rv.set_null(),
    }
}
fn cb_property_name(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    let parts = with_state(scope, sheet, |st, id| cssom::property_parts(st, id, &path));
    let v = str_val(scope, &parts.0);
    rv.set(v);
}
fn cb_property_syntax(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    let parts = with_state(scope, sheet, |st, id| cssom::property_parts(st, id, &path));
    let v = str_val(scope, &parts.1);
    rv.set(v);
}
fn cb_property_inherits(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    let parts = with_state(scope, sheet, |st, id| cssom::property_parts(st, id, &path));
    rv.set_bool(parts.2);
}
fn cb_property_initial(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    match with_state(scope, sheet, |st, id| cssom::property_parts(st, id, &path)).3 {
        Some(s) => {
            let v = str_val(scope, &s);
            rv.set(v);
        }
        None => rv.set_null(),
    }
}
fn cb_page_selector_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    let s = with_state(scope, sheet, |st, id| cssom::page_selector(st, id, &path));
    let v = str_val(scope, &s);
    rv.set(v);
}
/// `selectorText`'s setter on `@page`: a valid `<page-selector>` replaces the rule's; anything else
/// is ignored (Blink's `CSSPageRule::setSelectorText`).
fn cb_page_selector_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    let Some(text) = args.get(0).to_string(scope) else { return };
    let text = text.to_rust_string_lossy(scope);
    with_state(scope, sheet, |st, id| cssom::set_page_selector(st, id, &path, &text));
}

pub fn install_css_container_rule(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::install::live::getter_row;
    match member.name {
        "containerName" => getter_row(scope, proto, member, cb_container_name),
        "containerQuery" => getter_row(scope, proto, member, cb_container_query),
        "conditions" => getter_row(scope, proto, member, cb_container_conditions),
        other => debug_assert!(false, "install_css_container_rule: unexpected member {other}"),
    }
}

pub fn install_css_scope_rule(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::install::live::getter_row;
    match member.name {
        "start" => getter_row(scope, proto, member, cb_scope_start),
        "end" => getter_row(scope, proto, member, cb_scope_end),
        other => debug_assert!(false, "install_css_scope_rule: unexpected member {other}"),
    }
}

pub fn install_css_property_rule(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::install::live::getter_row;
    match member.name {
        "name" => getter_row(scope, proto, member, cb_property_name),
        "syntax" => getter_row(scope, proto, member, cb_property_syntax),
        "inherits" => getter_row(scope, proto, member, cb_property_inherits),
        "initialValue" => getter_row(scope, proto, member, cb_property_initial),
        other => debug_assert!(false, "install_css_property_rule: unexpected member {other}"),
    }
}

pub fn install_css_page_rule(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::install::live::pair_row;
    match member.name {
        "selectorText" => pair_row(scope, proto, member, cb_page_selector_get, cb_page_selector_set),
        "style" => { let g = crate::binding::protos::build_fn(scope, cb_style_rule_style_get, "get style", 0); crate::install::kits::put_forwards(scope, proto, member, g, "cssText") }
        other => debug_assert!(false, "install_css_page_rule: unexpected member {other}"),
    }
}

/// `nameList` -- a frozen array of the names, as Chrome answers (`["a", "b.c"]`).
fn cb_layer_name_list(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    let names = with_state(scope, sheet, |st, id| cssom::layer_name_list(st, id, &path));
    let items: Vec<v8::Local<v8::Value>> = names.iter().map(|n| str_val(scope, n)).collect();
    let arr = v8::Array::new_with_elements(scope, &items);
    arr.set_integrity_level(scope, v8::IntegrityLevel::Frozen);
    rv.set(arr.into());
}

// ── `CSSImportRule` ──────────────────────────────────────────────────────────────────────────────

fn import_info_of(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> Option<(v8::Global<v8::Object>, Vec<u32>, (String, String, Option<String>, Option<String>, Option<String>))> {
    let (sheet, path) = rule_recv(scope, args.this())?;
    let info = with_state(scope, sheet, |st, id| cssom::import_info(st, id, &path))?;
    Some((v8::Global::new(scope, sheet), path, info))
}

fn cb_import_href(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some((_, _, info)) = import_info_of(scope, &args) else { return };
    let v = str_val(scope, &info.0);
    rv.set(v);
}
fn cb_import_layer_name(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some((_, _, info)) = import_info_of(scope, &args) else { return };
    match info.2 {
        Some(n) => {
            let v = str_val(scope, &n);
            rv.set(v);
        }
        None => rv.set_null(),
    }
}
fn cb_import_supports_text(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some((_, _, info)) = import_info_of(scope, &args) else { return };
    match info.3 {
        Some(n) => {
            let v = str_val(scope, &n);
            rv.set(v);
        }
        None => rv.set_null(),
    }
}
/// `styleSheet` -- the imported sheet once its bytes have arrived, `[SameObject]`; null before.
fn cb_import_style_sheet<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some((sheet, path, info)) = import_info_of(scope, &args) else { return };
    let Some(url) = info.4 else {
        rv.set_null();
        return;
    };
    if let Some(s) = priv_get(scope, args.this(), RULE_IMPORTED).filter(|v| v.is_object()) {
        rv.set(s);
        return;
    }
    let sheet = v8::Local::new(scope, &sheet);
    if let Some(s) = crate::core::css::style_sheet::imported_sheet(scope, sheet, args.this(), &path, &url) {
        priv_set(scope, args.this(), RULE_IMPORTED, s.into());
        rv.set(s.into());
    }
}

// ── `CSSKeyframesRule` / `CSSKeyframeRule` ───────────────────────────────────────────────────────

fn cb_keyframes_length(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    let n = with_state(scope, sheet, |st, id| cssom::list_len(st, id, &path));
    rv.set_uint32(n);
}

/// `appendRule(rule)`: a keyframe block appended at the end; text that is not one is ignored, as
/// in Chrome (no exception).
fn cb_keyframes_append(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    let text = args.get(0).to_rust_string_lossy(scope);
    let n = with_state(scope, sheet, |st, id| cssom::list_len(st, id, &path));
    if with_state(scope, sheet, |st, id| cssom::insert_rule_at(st, id, &path, &text, n)) >= 0 {
        invalidate_children(scope, args.this());
    }
}

/// `deleteRule(select)`: the LAST keyframe with that selector goes; nothing happens otherwise.
fn cb_keyframes_delete(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    let key = args.get(0).to_rust_string_lossy(scope);
    if let Some(i) = with_state(scope, sheet, |st, id| cssom::find_keyframe(st, id, &path, &key)) {
        with_state(scope, sheet, |st, id| cssom::delete_rule_at(st, id, &path, i));
        invalidate_children(scope, args.this());
    }
}

/// `findRule(select)`: the last keyframe with that selector, or null.
fn cb_keyframes_find<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    let key = args.get(0).to_rust_string_lossy(scope);
    let Some(i) = with_state(scope, sheet, |st, id| cssom::find_keyframe(st, id, &path, &key)) else {
        rv.set_null();
        return;
    };
    let arr = children_of(scope, args.this(), sheet, &path);
    match arr.get_index(scope, i) {
        Some(r) => rv.set(r),
        None => rv.set_null(),
    }
}

fn cb_key_text_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    let t = with_state(scope, sheet, |st, id| cssom::key_text(st, id, &path));
    let v = str_val(scope, &t);
    rv.set(v);
}
/// A `keyText` that is not a keyframe selector list is a `SyntaxError` in Chrome.
fn cb_key_text_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    let text = args.get(0).to_rust_string_lossy(scope);
    if !with_state(scope, sheet, |st, id| cssom::set_key_text(st, id, &path, &text)) {
        let msg = format!("Failed to set the 'keyText' property on 'CSSKeyframeRule': The key '{text}' is invalid and cannot be parsed");
        throw_dom(scope, &msg, "SyntaxError");
    }
}

// ── `CSSPositionTryRule` bodies ──────────────────────────────────────────────────────────────────

/// `CSSPositionTryRule.style` — the `[SameObject]` `CSSPositionTryDescriptors`, minted once per rule.
fn cb_position_try_style_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    match block_of(scope, args.this(), sheet, &path, "CSSPositionTryDescriptors") {
        Some(b) => rv.set(b.into()),
        None => rv.set_undefined(),
    }
}

// ── the descriptor accessors (`CSSCounterStyleRule`, `CSSPositionTryDescriptors`) ────────────────

/// One descriptor off the receiver's rule, by the member's dashed CSS name (carried in the callback's
/// `data`, so ONE pair of callbacks serves all 80 descriptor rows).
fn cb_descriptor_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let s = with_state(scope, sheet, |st, id| cssom::descriptor(st, id, &path, &name));
    let v = str_val(scope, &s);
    rv.set(v);
}
fn cb_descriptor_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some((sheet, path)) = rule_recv(scope, args.this()) else { return };
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let value = args.get(0).to_rust_string_lossy(scope);
    // A value the grammar rejects is dropped silently; the page observes the drop by reading back.
    with_state(scope, sheet, |st, id| cssom::set_descriptor(st, id, &path, &name, &value));
}

/// Mount one descriptor row: a native getter/setter pair carrying the dashed CSS name as their data.
fn descriptor_row(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::binding::protos::build_fn_data;
    use crate::install::kits::define_accessor;
    let name = css_name(member.name);
    let d = str_val(scope, &name);
    let g = build_fn_data(scope, cb_descriptor_get, &format!("get {}", member.name), 0, d);
    let d2 = str_val(scope, &name);
    let s = build_fn_data(scope, cb_descriptor_set, &format!("set {}", member.name), 1, d2);
    define_accessor(scope, proto, member, g, Some(s));
}

// ── the row installers ───────────────────────────────────────────────────────────────────────────

pub fn install_css_rule(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::install::live::{getter_row, pair_row};
    match member.name {
        "type" => getter_row(scope, proto, member, cb_rule_type),
        "cssText" => pair_row(scope, proto, member, cb_rule_css_text_get, cb_rule_css_text_set),
        "parentRule" => getter_row(scope, proto, member, cb_rule_parent_rule),
        "parentStyleSheet" => getter_row(scope, proto, member, cb_rule_parent_sheet),
        other => debug_assert!(false, "install_css_rule: unexpected member {other}"),
    }
}

/// `CSSStyleRule.styleMap`: a `StylePropertyMap` over the rule's `style`, the same object every read.
fn cb_style_rule_style_map<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let this = args.this();
    if rule_recv(scope, this).is_none() {
        return;
    }
    if let Some(m) = crate::core::css::style_property_map::rule_map(scope, this) {
        rv.set(m.into());
    }
}

pub fn install_css_style_rule(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::install::live::{getter_row, method_row, pair_row};
    match member.name {
        "selectorText" => pair_row(scope, proto, member, cb_style_rule_selector_get, cb_style_rule_selector_set),
        "style" => { let g = crate::binding::protos::build_fn(scope, cb_style_rule_style_get, "get style", 0); crate::install::kits::put_forwards(scope, proto, member, g, "cssText") }
        "styleMap" => getter_row(scope, proto, member, cb_style_rule_style_map),
        "cssRules" => getter_row(scope, proto, member, cb_group_css_rules),
        "insertRule" => method_row(scope, proto, member, cb_group_insert_rule),
        "deleteRule" => method_row(scope, proto, member, cb_group_delete_rule),
        other => debug_assert!(false, "install_css_style_rule: unexpected member {other}"),
    }
}

pub fn install_css_grouping_rule(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::install::live::{getter_row, method_row};
    match member.name {
        "cssRules" => getter_row(scope, proto, member, cb_group_css_rules),
        "insertRule" => method_row(scope, proto, member, cb_group_insert_rule),
        "deleteRule" => method_row(scope, proto, member, cb_group_delete_rule),
        other => debug_assert!(false, "install_css_grouping_rule: unexpected member {other}"),
    }
}

pub fn install_css_condition_rule(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::install::live::{getter_row, pair_row};
    match member.name {
        "conditionText" => getter_row(scope, proto, member, cb_condition_text),
        "media" => pair_row(scope, proto, member, cb_rule_media_get, cb_rule_media_set),
        other => debug_assert!(false, "install_css_condition_rule: unexpected member {other}"),
    }
}

pub fn install_css_layer_rule(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::install::live::getter_row;
    match member.name {
        "name" => getter_row(scope, proto, member, cb_at_rule_name_get),
        "nameList" => getter_row(scope, proto, member, cb_layer_name_list),
        other => debug_assert!(false, "install_css_layer_rule: unexpected member {other}"),
    }
}

pub fn install_css_import_rule(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::install::live::{getter_row, pair_row};
    match member.name {
        "href" => getter_row(scope, proto, member, cb_import_href),
        "media" => pair_row(scope, proto, member, cb_rule_media_get, cb_rule_media_set),
        "styleSheet" => getter_row(scope, proto, member, cb_import_style_sheet),
        "layerName" => getter_row(scope, proto, member, cb_import_layer_name),
        "supportsText" => getter_row(scope, proto, member, cb_import_supports_text),
        other => debug_assert!(false, "install_css_import_rule: unexpected member {other}"),
    }
}

pub fn install_css_keyframes_rule(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::install::live::{getter_row, method_row, pair_row};
    match member.name {
        "name" => pair_row(scope, proto, member, cb_at_rule_name_get, cb_at_rule_name_set),
        "cssRules" => getter_row(scope, proto, member, cb_group_css_rules),
        "appendRule" => method_row(scope, proto, member, cb_keyframes_append),
        "deleteRule" => method_row(scope, proto, member, cb_keyframes_delete),
        "findRule" => method_row(scope, proto, member, cb_keyframes_find),
        "length" => getter_row(scope, proto, member, cb_keyframes_length),
        other => debug_assert!(false, "install_css_keyframes_rule: unexpected member {other}"),
    }
}

/// `style` of a keyframe, a font face, or nested declarations -- a `CSSStyleDeclaration` over the
/// rule's block, `[SameObject]`; `keyText` of a keyframe.
pub fn install_css_block_rule(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::install::live::{getter_row, pair_row};
    let readonly = matches!(member.kind, crate::install::MemberKind::Attr { readonly: true });
    match (member.name, readonly) {
        ("style", true) => getter_row(scope, proto, member, cb_style_rule_style_get),
        ("style", false) => pair_row(scope, proto, member, cb_style_rule_style_get, cb_style_rule_style_set),
        ("keyText", _) => pair_row(scope, proto, member, cb_key_text_get, cb_key_text_set),
        (other, _) => debug_assert!(false, "install_css_block_rule: unexpected member {other}"),
    }
}

pub fn install_css_position_try_rule(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::install::live::getter_row;
    match member.name {
        "name" => getter_row(scope, proto, member, cb_at_rule_name_get),
        "style" => { let g = crate::binding::protos::build_fn(scope, cb_position_try_style_get, "get style", 0); crate::install::kits::put_forwards(scope, proto, member, g, "cssText") }
        other => debug_assert!(false, "install_css_position_try_rule: unexpected member {other}"),
    }
}

pub fn install_css_counter_style_rule(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::install::live::pair_row;
    match member.name {
        // A counter style's `name` is not a descriptor — it is the rule's own name, and the only one
        // of the two at-rule names a page may assign to.
        "name" => pair_row(scope, proto, member, cb_at_rule_name_get, cb_at_rule_name_set),
        _ => descriptor_row(scope, proto, member),
    }
}

pub fn install_css_position_try_descriptors(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    descriptor_row(scope, proto, member)
}

/// THE GROUPING AND AT-RULE INTERFACES ARE REGISTRY ROWS WITH A KIT, not tables:
/// `CSSGroupingRule`, `CSSConditionRule`, `CSSMediaRule`, `CSSSupportsRule`, `CSSLayerBlockRule`,
/// `CSSLayerStatementRule`, `CSSImportRule`, `CSSKeyframesRule`, `CSSKeyframeRule`,
/// `CSSFontFaceRule` and `CSSNestedDeclarations` each sit in the capture's own-global order, and a
/// table installs its global where the CSSOM lane runs -- measured, turning these eleven into tables
/// moved eleven names out of place (`global_order` 531 -> 542). A kit keeps the registry's position
/// and gives the rows their bodies, which is how `OffscreenCanvas` and `CSSAnimation` got theirs.
/// `tools/sync/gen_shells.py` names every member this claims; `install/kits.rs::css_rule` forwards.
pub(crate) fn install_rule_kit(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.label.split("::").next().unwrap_or("") {
        "CSSGroupingRule" => install_css_grouping_rule(scope, proto, member),
        "CSSConditionRule" | "CSSMediaRule" => install_css_condition_rule(scope, proto, member),
        "CSSLayerBlockRule" | "CSSLayerStatementRule" => install_css_layer_rule(scope, proto, member),
        "CSSImportRule" => install_css_import_rule(scope, proto, member),
        "CSSKeyframesRule" => install_css_keyframes_rule(scope, proto, member),
        "CSSKeyframeRule" | "CSSFontFaceRule" | "CSSNestedDeclarations" => install_css_block_rule(scope, proto, member),
        "CSSContainerRule" => install_css_container_rule(scope, proto, member),
        "CSSScopeRule" => install_css_scope_rule(scope, proto, member),
        "CSSPropertyRule" => install_css_property_rule(scope, proto, member),
        "CSSPageRule" => install_css_page_rule(scope, proto, member),
        other => debug_assert!(false, "install_rule_kit: unexpected interface {other}"),
    }
}

/// The descriptor names `set_block_text` clears before re-parsing — the two tables' own member lists,
/// so the list cannot drift from the rows.
const POSITION_TRY_DESCRIPTOR_NAMES: &[&str] = &[
    "margin", "marginTop", "marginRight", "marginBottom", "marginLeft", "marginBlock",
    "marginBlockStart", "marginBlockEnd", "marginInline", "marginInlineStart", "marginInlineEnd",
    "inset", "insetBlock", "insetBlockStart", "insetBlockEnd", "insetInline", "insetInlineStart",
    "insetInlineEnd", "top", "left", "right", "bottom", "width", "minWidth", "maxWidth", "height",
    "minHeight", "maxHeight", "blockSize", "minBlockSize", "maxBlockSize", "inlineSize",
    "minInlineSize", "maxInlineSize", "placeSelf", "alignSelf", "justifySelf", "positionAnchor",
    "positionArea",
];
const COUNTER_STYLE_DESCRIPTOR_NAMES: &[&str] = &[
    "system", "symbols", "additiveSymbols", "negative", "prefix", "suffix", "range", "pad",
    "speakAs", "fallback",
];
/// CSS Paged Media 3's `@page` descriptors this CSSOM reads back.
const PAGE_DESCRIPTOR_NAMES: &[&str] = &[
    "margin", "marginTop", "marginRight", "marginBottom", "marginLeft", "size", "pageOrientation",
];
/// CSS Fonts 4 §4's `@font-face` descriptors.
const FONT_FACE_DESCRIPTOR_NAMES: &[&str] = &[
    "fontFamily", "src", "fontStyle", "fontWeight", "fontStretch", "fontDisplay", "unicodeRange",
    "fontFeatureSettings", "fontVariationSettings", "ascentOverride", "descentOverride",
    "lineGapOverride", "sizeAdjust",
];

// Rows VERBATIM from the `CSSRule`, `CSSStyleRule`, `CSSPositionTryRule`, `CSSCounterStyleRule` and
// `CSSPositionTryDescriptors` blocks of `install/generated_registry.rs`, in the capture's order. A
// WebIDL constant is carried TWICE — `constant` is the prototype's row, `static_constant` the
// interface object's — which is how the registry carries CSSRule's thirteen.
webapi::interface! {
    "CSSRule" [interface, window] {
        ctor     "CSSRule"                               len 0 = stub,
        attr     "type"             : Integer readonly         = install_css_rule,
        attr     "cssText"          : DomString                = install_css_rule,
        attr     "parentRule"       : Any readonly nullable    = install_css_rule,
        attr     "parentStyleSheet" : Any readonly nullable    = install_css_rule,
        constant "STYLE_RULE"                : Any = 1,
        constant "CHARSET_RULE"              : Any = 2,
        constant "IMPORT_RULE"               : Any = 3,
        constant "MEDIA_RULE"                : Any = 4,
        constant "FONT_FACE_RULE"            : Any = 5,
        constant "PAGE_RULE"                 : Any = 6,
        constant "MARGIN_RULE"               : Any = 9,
        constant "NAMESPACE_RULE"            : Any = 10,
        constant "KEYFRAMES_RULE"            : Any = 7,
        constant "KEYFRAME_RULE"             : Any = 8,
        constant "COUNTER_STYLE_RULE"        : Any = 11,
        constant "FONT_FEATURE_VALUES_RULE"  : Any = 14,
        constant "SUPPORTS_RULE"             : Any = 12,
        static_constant "STYLE_RULE"               : Any = 1,
        static_constant "CHARSET_RULE"             : Any = 2,
        static_constant "IMPORT_RULE"              : Any = 3,
        static_constant "MEDIA_RULE"               : Any = 4,
        static_constant "FONT_FACE_RULE"           : Any = 5,
        static_constant "PAGE_RULE"                : Any = 6,
        static_constant "MARGIN_RULE"              : Any = 9,
        static_constant "NAMESPACE_RULE"           : Any = 10,
        static_constant "KEYFRAMES_RULE"           : Any = 7,
        static_constant "KEYFRAME_RULE"            : Any = 8,
        static_constant "COUNTER_STYLE_RULE"       : Any = 11,
        static_constant "FONT_FEATURE_VALUES_RULE" : Any = 14,
        static_constant "SUPPORTS_RULE"            : Any = 12
    }
}

// `cssRules`, `deleteRule` and `insertRule` are the NESTED-rule half of `CSSStyleRule`, live since
// The parser keeps nested rules as children. `styleMap` is the Typed OM view of the
// block (`style_property_map::rule_map`).
webapi::interface! {
    "CSSStyleRule" : "CSSRule" [interface, window] {
        ctor   "CSSStyleRule"                        len 0 = stub,
        attr   "selectorText" : DomString                  = install_css_style_rule,
        attr   "style"        : Any                        = install_css_style_rule,
        attr   "styleMap"     : Any readonly               = install_css_style_rule,
        attr   "cssRules"     : Any readonly               = install_css_style_rule,
        method "deleteRule"   : Undefined len 1 required 1 = install_css_style_rule,
        method "insertRule"   : Integer   len 1 required 1 = install_css_style_rule
    }
}

webapi::interface! {
    "CSSPositionTryRule" : "CSSRule" [interface, window] {
        ctor "CSSPositionTryRule"            len 0 = stub,
        attr "name"  : DomString readonly          = install_css_position_try_rule,
        attr "style" : Any                         = install_css_position_try_rule
    }
}

webapi::interface! {
    "CSSCounterStyleRule" : "CSSRule" [interface, window] {
        ctor "CSSCounterStyleRule"                len 0 = stub,
        attr "name"            : DomString              = install_css_counter_style_rule,
        attr "system"          : DomString              = install_css_counter_style_rule,
        attr "symbols"         : DomString              = install_css_counter_style_rule,
        attr "additiveSymbols" : DomString              = install_css_counter_style_rule,
        attr "negative"        : DomString              = install_css_counter_style_rule,
        attr "prefix"          : DomString              = install_css_counter_style_rule,
        attr "suffix"          : DomString              = install_css_counter_style_rule,
        attr "range"           : DomString              = install_css_counter_style_rule,
        attr "pad"             : DomString              = install_css_counter_style_rule,
        attr "speakAs"         : DomString              = install_css_counter_style_rule,
        attr "fallback"        : DomString              = install_css_counter_style_rule
    }
}

webapi::interface! {
    "CSSPositionTryDescriptors" : "CSSStyleDeclaration" [interface, window] {
        ctor "CSSPositionTryDescriptors" len 0 = stub,
        attr "margin" : DomString = install_css_position_try_descriptors,
        attr "marginTop" : DomString = install_css_position_try_descriptors,
        attr "marginRight" : DomString = install_css_position_try_descriptors,
        attr "marginBottom" : DomString = install_css_position_try_descriptors,
        attr "marginLeft" : DomString = install_css_position_try_descriptors,
        attr "marginBlock" : DomString = install_css_position_try_descriptors,
        attr "marginBlockStart" : DomString = install_css_position_try_descriptors,
        attr "marginBlockEnd" : DomString = install_css_position_try_descriptors,
        attr "marginInline" : DomString = install_css_position_try_descriptors,
        attr "marginInlineStart" : DomString = install_css_position_try_descriptors,
        attr "marginInlineEnd" : DomString = install_css_position_try_descriptors,
        attr "margin-top" : Any = install_css_position_try_descriptors,
        attr "margin-right" : Any = install_css_position_try_descriptors,
        attr "margin-bottom" : Any = install_css_position_try_descriptors,
        attr "margin-left" : Any = install_css_position_try_descriptors,
        attr "margin-block" : Any = install_css_position_try_descriptors,
        attr "margin-block-start" : Any = install_css_position_try_descriptors,
        attr "margin-block-end" : Any = install_css_position_try_descriptors,
        attr "margin-inline" : Any = install_css_position_try_descriptors,
        attr "margin-inline-start" : Any = install_css_position_try_descriptors,
        attr "margin-inline-end" : Any = install_css_position_try_descriptors,
        attr "inset" : DomString = install_css_position_try_descriptors,
        attr "insetBlock" : DomString = install_css_position_try_descriptors,
        attr "insetBlockStart" : DomString = install_css_position_try_descriptors,
        attr "insetBlockEnd" : DomString = install_css_position_try_descriptors,
        attr "insetInline" : DomString = install_css_position_try_descriptors,
        attr "insetInlineStart" : DomString = install_css_position_try_descriptors,
        attr "insetInlineEnd" : DomString = install_css_position_try_descriptors,
        attr "top" : DomString = install_css_position_try_descriptors,
        attr "left" : DomString = install_css_position_try_descriptors,
        attr "right" : DomString = install_css_position_try_descriptors,
        attr "bottom" : DomString = install_css_position_try_descriptors,
        attr "inset-block" : Any = install_css_position_try_descriptors,
        attr "inset-block-start" : Any = install_css_position_try_descriptors,
        attr "inset-block-end" : Any = install_css_position_try_descriptors,
        attr "inset-inline" : Any = install_css_position_try_descriptors,
        attr "inset-inline-start" : Any = install_css_position_try_descriptors,
        attr "inset-inline-end" : Any = install_css_position_try_descriptors,
        attr "width" : DomString = install_css_position_try_descriptors,
        attr "minWidth" : DomString = install_css_position_try_descriptors,
        attr "maxWidth" : DomString = install_css_position_try_descriptors,
        attr "height" : DomString = install_css_position_try_descriptors,
        attr "minHeight" : DomString = install_css_position_try_descriptors,
        attr "maxHeight" : DomString = install_css_position_try_descriptors,
        attr "blockSize" : DomString = install_css_position_try_descriptors,
        attr "minBlockSize" : DomString = install_css_position_try_descriptors,
        attr "maxBlockSize" : DomString = install_css_position_try_descriptors,
        attr "inlineSize" : DomString = install_css_position_try_descriptors,
        attr "minInlineSize" : DomString = install_css_position_try_descriptors,
        attr "maxInlineSize" : DomString = install_css_position_try_descriptors,
        attr "min-width" : Any = install_css_position_try_descriptors,
        attr "max-width" : Any = install_css_position_try_descriptors,
        attr "min-height" : Any = install_css_position_try_descriptors,
        attr "max-height" : Any = install_css_position_try_descriptors,
        attr "block-size" : Any = install_css_position_try_descriptors,
        attr "min-block-size" : Any = install_css_position_try_descriptors,
        attr "max-block-size" : Any = install_css_position_try_descriptors,
        attr "inline-size" : Any = install_css_position_try_descriptors,
        attr "min-inline-size" : Any = install_css_position_try_descriptors,
        attr "max-inline-size" : Any = install_css_position_try_descriptors,
        attr "placeSelf" : DomString = install_css_position_try_descriptors,
        attr "alignSelf" : DomString = install_css_position_try_descriptors,
        attr "justifySelf" : DomString = install_css_position_try_descriptors,
        attr "place-self" : Any = install_css_position_try_descriptors,
        attr "align-self" : Any = install_css_position_try_descriptors,
        attr "justify-self" : Any = install_css_position_try_descriptors,
        attr "positionAnchor" : DomString = install_css_position_try_descriptors,
        attr "position-anchor" : Any = install_css_position_try_descriptors,
        attr "positionArea" : DomString = install_css_position_try_descriptors,
        attr "position-area" : Any = install_css_position_try_descriptors
    }
}
