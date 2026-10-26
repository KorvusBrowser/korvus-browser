//! `core/css/style_sheet.rs` — `StyleSheet`, `CSSStyleSheet` and `MediaList` as `interface!` tables.
//!
//! # What this replaces
//!
//! The kernel's `BOOTSTRAP` string literal (`core/korvus-runtime/src/lib.rs`) carried a ~300-line
//! CSSOM block. For these three interfaces it:
//!
//! * defined `cssRules`, `rules`, `insertRule`, `deleteRule`, `replaceSync` and `replace` straight
//!   onto `CSSStyleSheet.prototype` with `Object.defineProperty`, each body a `KB.__*` bridge slot
//!   wrapped in `op_helper_get` / `op_helper_method` — a native shell around a JS body, which is the
//!   `[native code]` mask invariant I3 deletes;
//! * defined `sheet` onto `HTMLStyleElement.prototype`, `HTMLLinkElement.prototype` and
//!   `SVGStyleElement.prototype` the same way, which is why `HTMLLinkElement` and `HTMLStyleElement`
//!   had to carry honest `stub` rows for `sheet` AND why their table installs were pinned to run
//!   BEFORE the bootstrap script (a define does not move an existing key, so the stub row put `sheet`
//!   in the capture's position and the kernel's later define supplied the body);
//! * minted each sheet as a plain object with SEVEN OWN DATA PROPERTIES — `ownerNode`, `type`,
//!   `disabled`, `href`, `title`, `media`, `__sheetId` — where a browser's `CSSStyleSheet` instance
//!   owns nothing at all and answers every one of them from `StyleSheet.prototype`. `__sheetId` and
//!   `__ruleCache` were page-visible: `Object.keys(document.styleSheets[0])` listed them.
//!
//! All of that is here now, as rows. The instance carries V8 **privates** (`Object.getOwnPropertyNames`
//! cannot see them, and neither can `JSON.stringify`), and every member is a `build_fn` native.
//!
//! # `media` is a real `MediaList`
//!
//! `mkSheet` used to answer `{ length: 0, mediaText: "", item: () => null }` — a plain object literal
//! wearing no prototype, so `sheet.media instanceof MediaList` was false and its three members were JS
//! closures. It is a `MediaList` platform object now, over the SHEET's own media list -- the one the
//! cascade evaluates -- which a `<style>`/`<link>`'s `media` attribute initialises and
//! `CSSStyleSheetInit.media` sets for a constructed sheet. So `<style media="print">` reports `print`
//! and, since P12b, does not apply to the screen.

use deno_core::v8;
use deno_core::v8::MapFnTo;

use crate::binding::protos::{get_prop, shell_proto, str_val, throw_dom, throw_type_error};
use crate::core::css::cssom;
use crate::core::dom::collections::static_list_reuse;
use crate::core::dom::element::priv_set;
use crate::core::css::css_style_declaration::priv_get;
use crate::install::MemberDef;

// ── the instance state, in V8 privates ───────────────────────────────────────────────────────────
//
// A private is not a property: `Object.getOwnPropertyNames`, `Reflect.ownKeys`, `JSON.stringify` and
// a `for…in` all miss it, and there is no string a page could guess. That is what makes it the right
// home for the six things a sheet has to remember, all of which used to be own properties.

/// The `korvus-page` handle this sheet's rules live behind (`cssom::sheet_new`).
pub(crate) const SHEET_ID: &str = "korvus_sheet_id";
/// The `<style>` / `<link>` element this sheet belongs to (`StyleSheet.ownerNode`), if any.
const SHEET_OWNER: &str = "korvus_sheet_owner";
/// The `<style>` element's text as it was when this sheet was last re-parsed from it.
const SHEET_SRC: &str = "korvus_sheet_src";
/// `StyleSheet.disabled` — a per-sheet flag, false until set.
const SHEET_DISABLED: &str = "korvus_sheet_disabled";
/// The `[SameObject]` `MediaList` for `StyleSheet.media`.
const SHEET_MEDIA: &str = "korvus_sheet_media";
/// A constructed sheet's media text (an element-backed sheet reads its owner's `media` attribute).
const SHEET_MEDIA_TEXT: &str = "korvus_sheet_media_text";
/// Set on a sheet that came from `new CSSStyleSheet()`. `replaceSync`/`replace` are allowed only on
/// one -- see [`cb_sheet_replace_sync`].
const SHEET_CONSTRUCTED: &str = "korvus_sheet_constructed";
/// The rule WRAPPER cache: one `CSSStyleRule`/at-rule object per index, so
/// `sheet.cssRules[0] === sheet.cssRules[0]`. Dropped whenever an edit can have moved an index.
pub(crate) const SHEET_RULES: &str = "korvus_sheet_rules";
/// The `CSSRuleList` over that cache, refilled in place so `sheet.cssRules === sheet.cssRules`.
const SHEET_RULELIST: &str = "korvus_sheet_rulelist";
/// The `CSSStyleSheet` an element owns, on the element's own wrapper.
const EL_SHEET: &str = "korvus_el_sheet";
/// A `MediaList`'s sheet.
const MEDIA_SHEET: &str = "korvus_media_sheet";
/// An IMPORTED sheet: the path of the `@import` rule, in its handle, whose children are
/// this sheet's rules. Absent on every other sheet, whose rules are the handle's top-level list.
const SHEET_PREFIX: &str = "korvus_sheet_prefix";
/// An imported sheet's `ownerRule` (the `CSSImportRule`) and `parentStyleSheet`.
const SHEET_OWNER_RULE: &str = "korvus_sheet_owner_rule";
const SHEET_PARENT: &str = "korvus_sheet_parent";
/// An imported sheet's resolved URL (`href`), and a linked sheet's: the URL its bytes came from.
const SHEET_HREF: &str = "korvus_sheet_href";
/// A DETACHED sheet's `title`, as its owner had it when the sheet was detached ([`detach_element_sheet`]).
const SHEET_TITLE: &str = "korvus_sheet_title";
/// An element-owned sheet's owner's `title` as of its last write while connected ([`title_written`]): what the
/// sheet keeps when it is detached, whatever the removed element's `title` says by the time anyone asks.
const SHEET_OWNER_TITLE: &str = "korvus_sheet_owner_title";
/// A DISABLED element-owned sheet's own row in the cascade, emptied while its rules wait in a page handle of their own
/// ([`set_sheet_disabled`]); absent on every other sheet.
const SHEET_HOME: &str = "korvus_sheet_home";

/// `cssom::sheet_new` answers this when the page plugin is not loaded, so there is nothing to parse
/// with. Every read degrades to "no rules" and every edit to a silent no-op, which is the state an
/// engine built without `korvus-page` has to keep working in.
const NO_PIPELINE: u32 = u32::MAX;
/// The bit a DETACHED sheet's [`SHEET_ID`] carries over its [`DetachedSheets`] key until its rules are asked for
/// ([`sheet_handle`]). A page handle is an index into the page's sheet list and never comes near it.
const DETACHED: u32 = 1 << 31;
/// An IMPORTED sheet's [`SHEET_ID`]: its rules are in its parent's handle, whichever that is now ([`sheet_handle`]) --
/// a parent that is detached or disabled moves to another, and an imported sheet that had kept the parent's handle of
/// its making read the element's next sheet, or an emptied row.
const FOLLOW_PARENT: u32 = u32::MAX - 1;

// ── the brand ────────────────────────────────────────────────────────────────────────────────────

/// The receiver of a `StyleSheet` / `CSSStyleSheet` row, or `None` with `Illegal invocation` thrown.
///
/// THE BRAND IS AN INTERNAL SLOT, not the prototype chain, which is what Chrome checks: measured on
/// Chrome 151, `CSSStyleSheet.prototype.insertRule.call({}, "a{}")` and
/// `Object.create(CSSStyleSheet.prototype).cssRules` both answer `TypeError: Illegal invocation`. The
/// deleted glue answered `undefined`/0 for both, and `cssInsertRule.call({})` did real work against
/// `undefined.__sheetId`. `SHEET_ID` is that slot: only [`new_sheet`] and the constructor write it.
fn sheet_ok(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> bool {
    if priv_get(scope, this, SHEET_ID).is_some() {
        return true;
    }
    throw_type_error(scope, "Illegal invocation");
    false
}

/// This sheet's `korvus-page` handle. A sheet always has one (the constructor and [`new_sheet`] both
/// allocate eagerly); `NO_PIPELINE` means there is no parser behind it. A DETACHED sheet's rules wait off the page's
/// sheet list ([`DetachedSheets`]) and are given a handle here, the first time anything asks for one.
pub(crate) fn sheet_handle(scope: &mut v8::PinScope, sheet: v8::Local<v8::Object>) -> u32 {
    let id = priv_get(scope, sheet, SHEET_ID).and_then(|v| v.uint32_value(scope)).unwrap_or(NO_PIPELINE);
    if id == NO_PIPELINE {
        return id;
    }
    if id == FOLLOW_PARENT {
        return match priv_get(scope, sheet, SHEET_PARENT).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
            Some(parent) => sheet_handle(scope, parent),
            None => NO_PIPELINE,
        };
    }
    if id & DETACHED != 0 {
        return materialize(scope, sheet, id & !DETACHED);
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    touch(&mut op_state.borrow_mut(), id);
    id
}

/// The path, in this sheet's handle, of the list this sheet's `cssRules` are: empty for a sheet, the
/// `@import` rule's path for an imported one.
pub(crate) fn sheet_prefix(scope: &mut v8::PinScope, sheet: v8::Local<v8::Object>) -> Vec<u32> {
    priv_get(scope, sheet, SHEET_PREFIX)
        .map(|v| v.to_rust_string_lossy(scope))
        .map(|t| t.split('.').filter_map(|p| p.parse().ok()).collect())
        .unwrap_or_default()
}

/// How many rules this sheet has, through the pipeline.
pub(crate) fn sheet_rule_count(scope: &mut v8::PinScope, sheet: v8::Local<v8::Object>) -> u32 {
    let id = sheet_handle(scope, sheet);
    if id == NO_PIPELINE {
        return 0;
    }
    let prefix = sheet_prefix(scope, sheet);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    cssom::list_len(&mut st, id, &prefix)
}

/// `CSSImportRule.styleSheet`: a `CSSStyleSheet` over the import rule's children in the parent's
/// handle. It has no owner node; its `href` is the URL it was fetched from, its `parentStyleSheet`
/// the importing sheet and its `ownerRule` the import rule -- all measured on Chrome 150.
pub(crate) fn imported_sheet<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    parent: v8::Local<'s, v8::Object>,
    rule: v8::Local<'s, v8::Object>,
    path: &[u32],
    url: &str,
) -> Option<v8::Local<'s, v8::Object>> {
    let proto = shell_proto(scope, "CSSStyleSheet")?;
    let obj = crate::binding::protos::mint_on(scope, proto.into());
    let idv: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, FOLLOW_PARENT).into();
    priv_set(scope, obj, SHEET_ID, idv);
    let p = str_val(scope, &path.iter().map(|i| i.to_string()).collect::<Vec<_>>().join("."));
    priv_set(scope, obj, SHEET_PREFIX, p);
    priv_set(scope, obj, SHEET_OWNER_RULE, rule.into());
    priv_set(scope, obj, SHEET_PARENT, parent.into());
    let h = str_val(scope, url);
    priv_set(scope, obj, SHEET_HREF, h);
    Some(obj)
}

/// A `MediaList` over a fixed query text that belongs to no sheet -- `CSSMediaRule.media` and
/// `CSSImportRule.media`. The holder it reads is an internal object nothing else can reach.
pub(crate) fn detached_media_list<'s>(scope: &mut v8::PinScope<'s, '_>, text: &str) -> Option<v8::Local<'s, v8::Object>> {
    let proto = shell_proto(scope, "MediaList")?;
    let holder = v8::Object::new(scope);
    let t = str_val(scope, text);
    priv_set(scope, holder, SHEET_MEDIA_TEXT, t);
    let m = crate::binding::protos::mint_on(scope, proto.into());
    priv_set(scope, m, MEDIA_SHEET, holder.into());
    Some(m)
}

/// May script reach INTO this sheet? A cross-origin sheet fetched without CORS is **not
/// origin-clean**: the page is styled by it and may not read it, and every member that would expose
/// a rule throws `SecurityError` instead. Returns `false` HAVING THROWN, so a caller returns at once.
///
/// Measured on stock Chrome 150.0.7871.46, two loopback origins, verbatim:
///
/// ```text
/// cssRules     SecurityError: Failed to read the 'cssRules' property from 'CSSStyleSheet': Cannot access rules
/// insertRule   SecurityError: Failed to execute 'insertRule' on 'CSSStyleSheet': Cannot access StyleSheet to insertRule
/// ```
///
/// `href`, `ownerNode`, `disabled`, `media` and `title` stay readable on the same sheet — they are
/// facts about the LINK, not about the bytes — and the rules still apply to the cascade. 52 of 142
/// sampled corpus pages carry such a sheet, so answering `[]` here rather than throwing would quietly
/// turn a page Chrome cannot read into a page Korvus reports rules for.
fn rules_readable(scope: &mut v8::PinScope, sheet: v8::Local<v8::Object>, msg: &str) -> bool {
    let id = sheet_handle(scope, sheet);
    if id == NO_PIPELINE {
        return true;
    }
    // An IMPORTED sheet is readable as its own response is ([`crate::core::css::sheet_loads::import_readable`]).
    let imported = priv_get(scope, sheet, SHEET_PREFIX).and_then(|_| priv_get(scope, sheet, SHEET_HREF)).map(|h| h.to_rust_string_lossy(scope));
    let clean = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        cssom::sheet_origin_clean(&mut st, id) && imported.as_deref().is_none_or(|h| crate::core::css::sheet_loads::import_readable(&st, h))
    };
    if !clean {
        throw_dom(scope, msg, "SecurityError");
    }
    clean
}

/// Throw away the rule-wrapper cache. An insert or a delete shifts every index after it, so a stale
/// wrapper would point at the wrong rule — the reason the deleted glue rebuilt on a length change.
pub(crate) fn invalidate_rules(scope: &mut v8::PinScope, sheet: v8::Local<v8::Object>) {
    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
    priv_set(scope, sheet, SHEET_RULES, u);
}

// ── minting a sheet ──────────────────────────────────────────────────────────────────────────────

/// A fresh `CSSStyleSheet` over a fresh empty `korvus-page` sheet, owned by `owner` (an element
/// wrapper) or by nothing (`new CSSStyleSheet()`).
fn new_sheet<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    owner: Option<v8::Local<'s, v8::Value>>,
) -> Option<v8::Local<'s, v8::Object>> {
    let proto = shell_proto(scope, "CSSStyleSheet")?;
    let obj = crate::binding::protos::mint_on(scope, proto.into());
    let id = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        cssom::sheet_new(&mut st, "")
    };
    let idv: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, id).into();
    priv_set(scope, obj, SHEET_ID, idv);
    if let Some(o) = owner {
        priv_set(scope, obj, SHEET_OWNER, o);
    }
    Some(obj)
}

/// `StyleSheet.ownerNode` — the element, or `None` for a constructed sheet and for one its element no longer
/// has: an element removed from its document, or a link no longer a stylesheet, has no sheet, and the one it had
/// is detached the moment that is seen ([`detach_element_sheet`]).
fn owner_of<'s>(scope: &mut v8::PinScope<'s, '_>, sheet: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    let owner = priv_get(scope, sheet, SHEET_OWNER).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let id = crate::binding::protos::unwrap_id(scope, owner.into());
    let allowed = id.is_some_and(|id| {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        sheet_allowed_in(&st, id)
    });
    let owned = allowed && priv_get(scope, owner, EL_SHEET).is_some_and(|s| s.strict_equals(sheet.into()));
    if !owned {
        if let Some(id) = id {
            detach_element_sheet(scope, id, Fresh::Own);
        }
        return None;
    }
    Some(owner)
}

/// One content attribute off the owner element, or `None`.
fn owner_attr(scope: &mut v8::PinScope, sheet: v8::Local<v8::Object>, name: &str) -> Option<String> {
    let owner = owner_of(scope, sheet)?;
    let k = str_val(scope, name);
    let v = crate::core::dom::element::attr_call(scope, owner.into(), "getAttribute", &[k])?;
    if v.is_null() || v.is_undefined() {
        return None;
    }
    Some(v.to_rust_string_lossy(scope))
}

// ── the element `sheet` getter (`HTMLStyleElement`, `HTMLLinkElement`) ───────────────────────────

/// `<style>.sheet` / `<link>.sheet` — the element's CSS style sheet.
///
/// A DISCONNECTED element has no CSS style sheet. HTML creates a `<style>`/`<link>`'s sheet when the
/// element "becomes connected" and removes it when it stops being connected, so a freshly created one
/// answers null — which is what the recorded browser says in three cases an earlier version minted a
/// sheet for and failed:
///
/// ```text
/// document.createElement("style").sheet                    -> null
/// document.createElementNS(svgNS, "style").sheet           -> null
/// document.createElement("link").sheet                     -> null
/// ```
///
/// A `<style>` element's sheet is a VIEW of its text, not a snapshot of it. Pages — and a good deal of
/// WPT — write `styleEl.firstChild.data = "…"` and read `.sheet` back expecting the new rules, so the
/// text is compared on every access and re-parsed when it has moved. Only for `<style>`: a `<link>`'s
/// rules come from the network, not from its (empty) text content, and re-syncing one would wipe
/// whatever the page had put in it.
///
/// A `<link>` THAT IS NOT A STYLESHEET HAS NO SHEET, and a `<link>`'s sheet is the one the cascade
/// holds for that element. Both measured on stock Chrome 150.0.7871.46:
///
/// ```text
/// <link href="/s.css">                  (no rel)  .sheet -> null
/// <link rel="alternate stylesheet" …>             .sheet -> a sheet, with its rules
/// <link rel="stylesheet" href="/s.css"> .sheet.cssRules.length -> 3, and #lk computes to 123px
/// ```
///
/// The last line is the defect P3a exists for: this getter used to mint a FRESH EMPTY sheet through
/// [`new_sheet`] for a `<link>` and never fill it, so `cssRules` was `[]` on every linked sheet on
/// the web. `cssom::sheet_for_element` hands back the entry `Page::sync_document_sheets` created for
/// the element in document order — the same one the bytes land in.
pub(crate) fn cb_element_sheet<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    match element_sheet(scope, args.this()) {
        Some(s) => rv.set(s.into()),
        None => rv.set_null(),
    }
}

/// The element's sheet, `None` for null. Everything it asks of the element it asks of the ARENA --
/// connectivity, local name, text -- never of page-visible getters: `document.styleSheets` runs this
/// for every `<style>`/`<link>`, and a bot-challenge frame, which reads `styleSheets` hundreds of
/// times, counted the `isConnected` / `localName` / `textContent` / `sheet` calls.
pub(crate) fn element_sheet<'s>(scope: &mut v8::PinScope<'s, '_>, el: v8::Local<'s, v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    let id = crate::binding::protos::unwrap_id(scope, el.into())?;
    // Both answers off the arena and the minted set in one borrow: `document.styleSheets` asks this of every
    // `<style>` and `<link>`, most of them icon, preload or preconnect links that never have a sheet, and a
    // `priv_get` of the element's sheet slot to learn that it had none cost each a Private lookup by name, on top of
    // an allocated local name and four separate arena reads. `styleSheets.length` over 16 such links, 3 sheet links
    // and 4 styles, instructions a read (2026-10-23): 67.3k before the detach, 78.3k with that `priv_get`, 59.8k now.
    let (allowed, minted) = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        (sheet_allowed_in(&st, id), minted_in(&st, id))
    };
    if !allowed {
        // The sheet it had, if any, is detached ([`detach_element_sheet`]).
        if minted {
            detach_element_sheet(scope, id, Fresh::Own);
        }
        return None;
    }
    let existing = if minted { priv_get(scope, el, EL_SHEET).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) } else { None };
    let sheet = match existing {
        Some(s) => s,
        None => {
            let s = mint_element_sheet(scope, el)?;
            priv_set(scope, el, EL_SHEET, s.into());
            s
        }
    };
    resync_from_text(scope, el, sheet);
    Some(sheet)
}

/// May element `id` have a CSS style sheet now? Connected, and a `<style>` or a stylesheet `<link>` -- one whose sheet
/// exists: not before its first response, nor for a link that cannot fetch -- no `href`, a blank or unparseable
/// one (measured in `core/html/subresource.rs` `LinkSheetFetches`). Off the arena, in the borrow the caller holds.
/// A stylesheet link is `page::is_stylesheet_link`'s, the ONE rule the cascade's sheet rows are made by, so an element
/// cannot have a CSSOM sheet the cascade has no row for.
fn sheet_allowed_in(st: &deno_core::OpState, id: u32) -> bool {
    let Some(h) = st.try_borrow::<crate::binding::HostDom>() else { return false };
    let dom = &h.0;
    if dom.node_type(dom.shadow_including_root(id)) != 9 {
        return false;
    }
    match dom.tag_name(id) {
        Some(t) if t.eq_ignore_ascii_case("link") => page::is_stylesheet_link(dom, id) && crate::core::html::subresource::link_sheet_may_exist_in(st, dom, id),
        Some(_) => true,
        None => false,
    }
}

/// The elements whose `sheet` has been minted ([`mint_element_sheet`]), by arena id: the ones that have a sheet to
/// detach when they leave their document or come back to one ([`detach_element_sheet`]).
#[derive(Default)]
pub(crate) struct MintedSheets(std::collections::HashSet<u32>);

/// A new document: its node ids are the last one's again ([`crate::core::html::subresource::reset_started_loads`]).
pub(crate) fn reset_minted_sheets(st: &mut deno_core::OpState) {
    if let Some(m) = st.try_borrow_mut::<MintedSheets>() {
        m.0.clear();
    }
}

/// Has element `id` a minted sheet to detach ([`MintedSheets`])?
pub(crate) fn has_minted_sheet(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>, id: u32) -> bool {
    minted_in(&op_state.borrow(), id)
}

/// [`has_minted_sheet`] in a borrow the caller holds.
pub(crate) fn minted_in(st: &deno_core::OpState, id: u32) -> bool {
    st.try_borrow::<MintedSheets>().is_some_and(|m| m.0.contains(&id))
}

/// What an element's own handle holds once its sheet is detached ([`detach_element_sheet`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Fresh {
    /// The sheet the element's next insertion makes: a `<style>`'s text parsed again, a link's last response.
    Own,
    /// No rules: a link whose next response fills them, or whose `href` can fetch nothing.
    Empty,
}

/// DETACHED sheets' rules, by key, held OFF the page's sheet list ([`detach_element_sheet`]) until script asks a
/// detached sheet for them ([`sheet_handle`] then gives them a page handle, [`materialize`]).
///
/// The first cut kept each detached sheet as a constructed copy in the page's list, and the list never shrinks:
/// every later sheet change rebuilt a matcher over all of them and every load scan walked them, so a `<style>`
/// whose sheet a page reads and that it moves -- JSS's `detach()`/`attach()` -- cost more each time. 1,500 moves of a
/// 501-rule `<style>` took 135 s and 427 MB where the build before took 0.18 s; Chrome 152 moves one 1,500 times in
/// 3 ms. Held here, a detached sheet costs its rules' memory and nothing else, and that memory goes when its
/// `CSSStyleSheet` does: each entry holds a weak handle to it, and the entries whose sheet V8 has collected are
/// dropped every 64 entries ([`hold_detached`]).
///
/// V8 collects a sheet only when its own heap asks for a collection, and a loop that moves an edited sheet allocates
/// little of that heap: 6,000 moves of a 501-rule `<style>` an `insertRule` wrote to held 300 MB of rules no sheet
/// could reach. Blink keeps a detached sheet's rules in its own garbage-collected heap, which collects as they grow;
/// here, rules held past [`HELD_BUDGET`] since the last collection ask V8 for one (a critical memory-pressure
/// notification, a full collection), then the table is swept. Not V8's external-memory count: that is part of
/// `performance.memory`'s heap sizes, where Chrome counts no stylesheet.
#[derive(Default)]
pub(crate) struct DetachedSheets {
    next: u32,
    held: std::collections::HashMap<u32, HeldSheet>,
    swept_at: usize,
    /// About how many bytes the parsed rules held hold ([`HeldRules::bytes`]), and the mark that asks for a collection.
    bytes: usize,
    collect_at: usize,
    /// The page handles sheet objects were given OFF their element's row ([`give_copy`]): a detached sheet's rules once
    /// asked for ([`materialize`]), a disabled sheet's ([`set_sheet_disabled`]). Each with a weak handle to its sheet;
    /// once V8 has collected that, nothing can address the handle again, and it is emptied and given to the next one
    /// ([`sweep_copies`]) -- the page's list never shrinks, so it is reused instead.
    copies: Vec<CopySheet>,
    copies_swept_at: usize,
    free: Vec<u32>,
}

/// How many bytes of held rules ask V8 for a collection ([`DetachedSheets`]).
const HELD_BUDGET: usize = 64 << 20;

struct CopySheet {
    handle: u32,
    owner: v8::Weak<v8::Object>,
    bytes: usize,
}

/// About what a parsed sheet holds on the heap ([`DetachedSheets`]).
fn parsed_bytes(s: &page::style::rules::Stylesheet) -> usize {
    s.text().len() * 2 + s.rules().len() * 256 + (s.declarations().len() + s.raw_declarations().len()) * 64
}

/// A page handle's emptied rules: no rules, styling nothing.
fn emptied() -> page::style::rules::Stylesheet {
    let mut s = page::style::parse::parse_stylesheet(page::style::rules::Origin::Author, "");
    s.disabled = true;
    s
}

struct HeldSheet {
    rules: HeldRules,
    owner: v8::Weak<v8::Object>,
}

/// A detached sheet's rules: parsed, as its handle had them, or -- when nothing had asked its handle for them since
/// they were parsed ([`TouchedSheets`]) -- the source they were parsed from, which is all they are.
enum HeldRules {
    Parsed(page::style::rules::Stylesheet),
    /// `row` is the element's handle the source was parsed into, which holds the parse still while nothing has touched
    /// it and its text is this one ([`materialize`]).
    Source { text: std::rc::Rc<str>, base: Option<String>, clean: bool, media: String, row: u32 },
}

impl HeldRules {
    /// About what these hold on the heap ([`DetachedSheets`]).
    fn bytes(&self) -> usize {
        match self {
            HeldRules::Parsed(s) => parsed_bytes(s),
            HeldRules::Source { text, .. } => text.len(),
        }
    }
}

/// Drop the held entries whose sheet V8 has collected.
fn sweep_detached(d: &mut DetachedSheets) {
    let mut freed = 0;
    d.held.retain(|_, h| {
        let live = !h.owner.is_empty();
        if !live {
            freed += h.rules.bytes();
        }
        live
    });
    d.bytes = d.bytes.saturating_sub(freed);
    d.swept_at = d.held.len();
    d.collect_at = d.collect_at.min(d.bytes + HELD_BUDGET);
}

/// Hold `rules` as detached sheet `owner`'s ([`DetachedSheets`]); the key its [`SHEET_ID`] carries.
fn hold_detached(scope: &mut v8::PinScope, owner: v8::Local<v8::Object>, rules: HeldRules) -> u32 {
    let weak = v8::Weak::new(scope, owner);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (key, collect) = {
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<DetachedSheets>().is_none() {
            st.put(DetachedSheets { collect_at: HELD_BUDGET, ..DetachedSheets::default() });
        }
        let d = st.borrow_mut::<DetachedSheets>();
        if d.held.len() >= d.swept_at + 64 {
            sweep_detached(d);
        }
        d.bytes += rules.bytes();
        // Keys stay below [`DETACHED`]; one wrapping round would need two billion detaches.
        let key = d.next;
        d.next = (d.next + 1) & !DETACHED;
        d.held.insert(key, HeldSheet { rules, owner: weak });
        (key, d.bytes >= d.collect_at)
    };
    if collect {
        collect_detached(scope);
    }
    key
}

/// Ask V8 for a collection -- now, or at its next interrupt check when the isolate is not locked; either way the sweeps
/// after it drop what it found -- and sweep; the next one waits for as much again ([`DetachedSheets`]).
fn collect_detached(scope: &mut v8::PinScope) {
    scope.memory_pressure_notification(v8::MemoryPressureLevel::Critical);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    let p = page_handle(&st);
    if let Some(p) = &p {
        sweep_copies(&mut st, p);
    }
    let d = st.borrow_mut::<DetachedSheets>();
    sweep_detached(d);
    d.collect_at = d.bytes + HELD_BUDGET;
}

fn page_handle(st: &deno_core::OpState) -> Option<std::sync::Arc<page::Page>> {
    st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<page::PageHandle>()).map(|h| h.0.clone())
}

/// Empty the copies whose sheet V8 has collected and keep their handles for the next ones ([`DetachedSheets`]).
fn sweep_copies(st: &mut deno_core::OpState, p: &page::Page) {
    let Some(d) = st.try_borrow_mut::<DetachedSheets>() else { return };
    let mut dead = Vec::new();
    let mut freed = 0;
    d.copies.retain(|c| {
        let live = !c.owner.is_empty();
        if !live {
            dead.push(c.handle);
            freed += c.bytes;
        }
        live
    });
    d.bytes = d.bytes.saturating_sub(freed);
    d.copies_swept_at = d.copies.len();
    if dead.is_empty() {
        return;
    }
    d.free.extend(&dead);
    for &h in &dead {
        p.with_sheet_mut(h, |s| *s = emptied());
        take_touched(st, h);
    }
    crate::core::css::sheet_loads::forget_sheets(st, &dead);
}

/// A page handle of its own for sheet object `owner`, holding `rules` (which the caller has disabled): a free one, else
/// a new constructed one no tree adopts ([`DetachedSheets`]).
fn give_copy(scope: &mut v8::PinScope, owner: v8::Local<v8::Object>, p: &page::Page, rules: page::style::rules::Stylesheet) -> u32 {
    let bytes = parsed_bytes(&rules);
    let weak = v8::Weak::new(scope, owner);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (handle, collect) = {
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<DetachedSheets>().is_none() {
            st.put(DetachedSheets { collect_at: HELD_BUDGET, ..DetachedSheets::default() });
        }
        let d = st.borrow_mut::<DetachedSheets>();
        if d.copies.len() >= d.copies_swept_at + 64 {
            sweep_copies(&mut st, p);
        }
        let d = st.borrow_mut::<DetachedSheets>();
        let handle = d.free.pop().unwrap_or_else(|| p.add_constructed_sheet(""));
        p.with_sheet_mut(handle, move |s| *s = rules);
        d.copies.push(CopySheet { handle, owner: weak, bytes });
        d.bytes += bytes;
        (handle, d.bytes >= d.collect_at)
    };
    if collect {
        collect_detached(scope);
    }
    handle
}

/// Copy `handle` is no longer `owner`'s: emptied, and given to the next one ([`DetachedSheets`]).
fn release_copy(st: &mut deno_core::OpState, p: &page::Page, handle: u32) {
    let Some(d) = st.try_borrow_mut::<DetachedSheets>() else { return };
    let Some(at) = d.copies.iter().position(|c| c.handle == handle) else { return };
    let c = d.copies.swap_remove(at);
    d.bytes = d.bytes.saturating_sub(c.bytes);
    d.copies_swept_at = d.copies_swept_at.min(d.copies.len());
    d.free.push(handle);
    p.with_sheet_mut(handle, |s| *s = emptied());
    take_touched(st, handle);
    crate::core::css::sheet_loads::forget_sheets(st, &[handle]);
}

/// Give detached sheet `sheet` (held under `key`) a page handle of its own: constructed (no tree adopts it) and
/// disabled, so it styles nothing and its `@font-face`s load nothing, its rules the ones it was detached with. Its
/// rule wrappers address it by path through the sheet object, so they are the same objects after this.
///
/// A handle that sheet's death frees for the next one ([`give_copy`]). The first cut added one to the page's list for
/// good each time: a 300-rule `<style>` moved 1,000 times, its old sheet's `cssRules` read after each move, left 1,000
/// copies every later matcher build and load scan walked -- the moves took 559 ms and the 2,000 restyles after them
/// 156 ms, where the build before took 4 and 19, and Chrome 152 3 and 5 (main thread, 2026-10-23).
fn materialize(scope: &mut v8::PinScope, sheet: v8::Local<v8::Object>, key: u32) -> u32 {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (held, p) = {
        let mut st = op_state.borrow_mut();
        let held = st.try_borrow_mut::<DetachedSheets>().and_then(|d| d.held.remove(&key));
        (held, page_handle(&st))
    };
    let Some(held) = held else { return NO_PIPELINE };
    if let Some(d) = op_state.borrow_mut().try_borrow_mut::<DetachedSheets>() {
        d.bytes = d.bytes.saturating_sub(held.rules.bytes());
    }
    let Some(p) = p else { return NO_PIPELINE };
    let (mut rules, imports) = match held.rules {
        HeldRules::Parsed(s) => (s, false),
        HeldRules::Source { text, base, clean, media, row } => {
            // The element's row while it is still that parse is copied rather than parsed again: a 300-rule `<style>`
            // moved and its old sheet read, 1,000 times, 0.56 ms a move parsing and 0.18 copying (2026-10-23).
            let untouched = !is_touched(&op_state.borrow(), row);
            let same = if untouched { p.with_sheet(row, |r| (r.text() == &*text).then(|| r.clone())).flatten() } else { None };
            let parsed = same.is_none();
            let mut s = same.unwrap_or_else(|| page::style::parse::parse_stylesheet(page::style::rules::Origin::Author, &text));
            s.base_url = base;
            s.origin_clean = clean;
            s.set_media_text(&media);
            let imports = parsed && has_import(&s);
            (s, imports)
        }
    };
    rules.disabled = true;
    let copy = give_copy(scope, sheet, &p, rules);
    let v: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, copy).into();
    priv_set(scope, sheet, SHEET_ID, v);
    // Its imports that had arrived are attached again from the bodies in hand ([`crate::core::css::sheet_loads`]).
    if imports {
        crate::core::css::sheet_loads::sheet_reparsed(scope);
    }
    copy
}

fn has_import(s: &page::style::rules::Stylesheet) -> bool {
    s.rules().iter().any(|r| matches!(r.kind, page::style::rules::RuleKind::Import(_)))
}

/// The page handles a sheet object has been asked for since their rules were last parsed from their source
/// ([`sheet_handle`]): every CSSOM read and write goes through that, so a handle not in here holds exactly what its
/// source parses to -- a `<style>`'s text, a link's response -- and a detached sheet over it can keep that source
/// instead of the rules ([`HeldRules::Source`]), and its element's next sheet is the handle as it is. A bit per handle.
#[derive(Default)]
pub(crate) struct TouchedSheets(Vec<u64>);

fn touch(st: &mut deno_core::OpState, handle: u32) {
    if st.try_borrow::<TouchedSheets>().is_none() {
        st.put(TouchedSheets::default());
    }
    let bits = &mut st.borrow_mut::<TouchedSheets>().0;
    let (word, bit) = ((handle / 64) as usize, handle % 64);
    if bits.len() <= word {
        bits.resize(word + 1, 0);
    }
    bits[word] |= 1 << bit;
}

/// Was `handle` asked for since its rules were parsed ([`TouchedSheets`])?
fn is_touched(st: &deno_core::OpState, handle: u32) -> bool {
    st.try_borrow::<TouchedSheets>().is_some_and(|t| t.0.get((handle / 64) as usize).is_some_and(|w| w & (1 << (handle % 64)) != 0))
}

/// Was `handle` asked for since its rules were parsed ([`TouchedSheets`])? It is not, from now on.
fn take_touched(st: &mut deno_core::OpState, handle: u32) -> bool {
    let Some(t) = st.try_borrow_mut::<TouchedSheets>() else { return false };
    let (word, bit) = ((handle / 64) as usize, handle % 64);
    t.0.get_mut(word).is_some_and(|w| std::mem::replace(w, *w & !(1 << bit)) & (1 << bit) != 0)
}

/// `handle`'s rules were just parsed afresh from its source -- a link's response arrived ([`TouchedSheets`]).
pub(crate) fn sheet_parsed_fresh(st: &mut deno_core::OpState, handle: u32) {
    take_touched(st, handle);
}

/// Element `id` no longer has the sheet it had: DETACH it, as HTML's "remove a CSS style sheet" does. Chrome
/// 152.0.7977.82, 2026-10-23, `<style title=tt media=screen>` and `<link title=lt media=screen>` whose sheets
/// script disabled:
///
/// ```text
///   removed                              ownerNode null; disabled true, its rules, title, media and href kept;
///                                        the element's `sheet` null
///   then the removed link's title/href   the sheet still answers `lt` and the URL it was loaded from
///   written
///   removed, re-inserted                 a NEW sheet (`sheet === old` false), from the element's text for a
///                                        `<style>`: an `insertRule` into the old one is in the old one only
///   moved in one step, a link's href     the same: a new sheet, the old one's ownerNode null
///   changed and loaded, rel/disabled
/// ```
///
/// This engine kept one sheet per element for the element's life: `ownerNode` still named the removed element,
/// the reinserted element answered the same object, and its CSSOM edits survived the move. The old sheet now TAKES
/// the rules its handle had, held off the page's list ([`DetachedSheets`]), and answers its `href` and `title` from
/// what its owner had; the element's handle -- the cascade's row -- gets `fresh`: [`Fresh::Own`] is its text parsed
/// again for a `<style>` and its last response for a link, which is what Chrome's next sheet holds -- a linked
/// sheet moved after an `insertRule` into it has its 2 rules again, not 3, and does not style with the inserted one
/// (Chrome 152, a cacheable and a `no-store` link, moved or disabled and enabled: `n2` 2, `#t` 50px; the first cut
/// kept the edited rules: 3, 77px). A handle nothing has asked for since it was parsed ([`TouchedSheets`]) already
/// is that, and the old sheet keeps only its source: a 501-rule `<style>` whose sheet is read and that is moved
/// costs no parse a move. Without a minted sheet nothing can have edited the handle, so only [`Fresh::Empty`] has
/// anything to do.
pub(crate) fn detach_element_sheet(scope: &mut v8::PinScope, id: u32, fresh: Fresh) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let minted = op_state.borrow_mut().try_borrow_mut::<MintedSheets>().is_some_and(|m| m.0.remove(&id));
    if !minted && fresh == Fresh::Own {
        return;
    }
    let el = crate::binding::protos::wrap_cached(scope, &op_state, id);
    let sheet = if minted { priv_get(scope, el, EL_SHEET).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) } else { None };
    // A disabled sheet's row, emptied -- its rules are in the copy it keeps ([`set_sheet_disabled`]).
    let home = sheet.and_then(|s| priv_get(scope, s, SHEET_HOME)).and_then(|v| v.uint32_value(scope));
    // The element's handle: the minted sheet's (not yet detached, so a page handle), else the cascade's row for a
    // stylesheet link -- a link that is no longer one has none worth making.
    let handle = match (sheet, home) {
        (Some(_), Some(h)) => h,
        (Some(s), None) => priv_get(scope, s, SHEET_ID).and_then(|v| v.uint32_value(scope)).unwrap_or(NO_PIPELINE),
        (None, _) => {
            let mut st = op_state.borrow_mut();
            let link = st.try_borrow::<crate::binding::HostDom>().is_some_and(|h| page::is_stylesheet_link(&h.0, id));
            if link { cssom::sheet_for_element(&mut st, id) } else { NO_PIPELINE }
        }
    };
    if let Some(sheet) = sheet {
        if priv_get(scope, sheet, SHEET_HREF).is_none() {
            let href: v8::Local<v8::Value> = match link_href(scope, el) {
                Some(h) => str_val(scope, &h),
                None => v8::null(scope).into(),
            };
            priv_set(scope, sheet, SHEET_HREF, href);
        }
        let title = priv_get(scope, sheet, SHEET_OWNER_TITLE).unwrap_or_else(|| v8::null(scope).into());
        priv_set(scope, sheet, SHEET_TITLE, title);
        let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
        priv_set(scope, sheet, SHEET_OWNER, undef);
        priv_set(scope, el, EL_SHEET, undef);
        priv_set(scope, sheet, SHEET_HOME, undef);
    }
    if handle == NO_PIPELINE || handle & DETACHED != 0 {
        return;
    }
    let Some(p) = op_state.borrow().try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<page::PageHandle>()).map(|h| h.0.clone()) else { return };
    let style = op_state.borrow().try_borrow::<crate::binding::HostDom>().is_some_and(|h| h.0.tag_name(id).is_some_and(|t| t.eq_ignore_ascii_case("style")));
    // An emptied row is refilled as an edited one is.
    let touched = take_touched(&mut op_state.borrow_mut(), handle) || home.is_some();
    // What the element's next sheet holds -- or, for a handle nobody touched, what the old sheet keeps: (text, its
    // base URL, origin-clean).
    let source: (std::rc::Rc<str>, Option<String>, bool) = match fresh {
        Fresh::Empty => ("".into(), None, true),
        // Untouched, the element keeps the handle and the sheet sync parses a changed text into it itself; the old
        // sheet keeps the text it was last synced from ([`SHEET_SRC`]).
        Fresh::Own if style && !touched => {
            let src = sheet.and_then(|s| priv_get(scope, s, SHEET_SRC)).map(|v| v.to_rust_string_lossy(scope));
            (src.unwrap_or_else(|| crate::page::child_text_content(scope, el.into())).into(), None, true)
        }
        // Touched, the handle is parsed again from the element's text AS IT IS NOW, after the sheet sync has seen
        // it -- so the sync's record of that text is this text and it parses nothing over it later. The first cut
        // parsed [`SHEET_SRC`], the text the sheet was last READ at: a `<style>` whose rules script had read, whose
        // text then changed and was laid out, and that was then moved, styled with its OLD text for good, since the
        // sync had already parsed the new one and saw nothing to do. Chrome 152.0.7977.82, 2026-10-23,
        // `S.sheet.cssRules[0]; S.textContent = '#t{width:20px}'; getComputedStyle(t); head.appendChild(S)`: `#t`
        // 20px and `S.sheet.cssRules[0]` `#t { width: 20px; }`; that cut, 10px and `#t { width: 10px; }`.
        Fresh::Own if style => {
            cssom::sheet_for_element(&mut op_state.borrow_mut(), id);
            (crate::page::child_text_content(scope, el.into()).into(), None, true)
        }
        Fresh::Own => crate::core::html::subresource::link_sheet_body(&op_state.borrow(), id).unwrap_or_else(|| ("".into(), None, true)),
    };
    if fresh == Fresh::Own && !touched {
        // The handle is its source parsed: the old sheet keeps the source, the element the handle as it is.
        if let Some(sheet) = sheet {
            let media = p.with_sheet(handle, |s| s.media_text().to_string()).unwrap_or_default();
            let (text, base, clean) = source;
            let key = hold_detached(scope, sheet, HeldRules::Source { text, base, clean, media, row: handle });
            let v: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, key | DETACHED).into();
            priv_set(scope, sheet, SHEET_ID, v);
        }
        return;
    }
    let (text, base, clean) = source;
    let mut next = page::style::parse::parse_stylesheet(page::style::rules::Origin::Author, &text);
    let imports = has_import(&next);
    next.origin_clean = clean;
    next.base_url = base;
    let Some(old) = p.with_sheet_mut(handle, move |s| {
        next.disabled = s.disabled;
        next.set_media_text(s.media_text());
        std::mem::replace(s, next)
    }) else {
        return;
    };
    if let Some(sheet) = sheet.filter(|_| home.is_none()) {
        let key = hold_detached(scope, sheet, HeldRules::Parsed(old));
        let v: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, key | DETACHED).into();
        priv_set(scope, sheet, SHEET_ID, v);
    }
    // The imports the text names that have arrived are put back from the bodies in hand ([`sheet_loads::scan`]); a
    // sheet with none -- nearly every `<style>` -- skips that whole-document scan.
    if imports {
        crate::core::css::sheet_loads::sheet_reparsed(scope);
    }
}

/// Element `id`'s `title` was written: while it is connected its sheet's `title` follows it (Blink's `SetSheetTitle`
/// for a `<link>`, `HTMLStyleElement::ParseAttribute` for a `<style>`), and that is the title the sheet keeps if it is
/// detached later ([`SHEET_OWNER_TITLE`]). Written while removed, it is not the sheet's.
pub(crate) fn title_written(scope: &mut v8::PinScope, id: u32) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if !has_minted_sheet(&op_state, id) {
        return;
    }
    let el = crate::binding::protos::wrap_cached(scope, &op_state, id);
    if !crate::page::is_connected(scope, el.into()) {
        return;
    }
    let Some(sheet) = priv_get(scope, el, EL_SHEET).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
    let title = op_state.borrow().try_borrow::<crate::binding::HostDom>().and_then(|h| h.0.get_attribute(id, "title").filter(|t| !t.is_empty()).map(str::to_string));
    let v: v8::Local<v8::Value> = match title {
        Some(t) => str_val(scope, &t),
        None => v8::null(scope).into(),
    };
    priv_set(scope, sheet, SHEET_OWNER_TITLE, v);
}

/// A `<link>`'s resolved `href` as its `href` IDL attribute reads it, off the arena, or `None` for no `<link>`
/// href (a `<style>`).
fn link_href(scope: &mut v8::PinScope, el: v8::Local<v8::Object>) -> Option<String> {
    let id = crate::binding::protos::unwrap_id(scope, el.into())?;
    let raw = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let h = st.try_borrow::<crate::binding::HostDom>()?;
        if !h.0.tag_name(id).is_some_and(|t| t.eq_ignore_ascii_case("link")) {
            return None;
        }
        h.0.get_attribute(id, "href")?.trim().to_string()
    };
    if raw.is_empty() {
        return None;
    }
    let base = crate::core::html::subresource::node_base_url(scope, id);
    Some(url::Url::parse(&base).ok().and_then(|b| b.join(&raw).ok()).map(|u| u.to_string()).unwrap_or(raw))
}

/// The `CSSStyleSheet` for an element, over the `korvus-page` sheet **that element already owns** —
/// the one the cascade is using, never a second one.
///
/// READING `.sheet` USED TO MOVE THE ELEMENT IN THE CASCADE, and this is where. The getter minted a
/// fresh page sheet (`new_sheet` -> `add_sheet(Author, None, "")`) and `resync_from_text` parsed the
/// element's text into it, while `Page::sync_document_sheets` already held one for the same element
/// at its document-order index. The rules were therefore in the cascade TWICE, the second copy
/// appended at the end where it outranks every sheet later in the markup. Reproduced in this file's
/// `reading_a_style_elements_sheet_does_not_move_it_in_the_cascade`: an inline `#ord2 { width: 40px }`
/// before a linked `#ord2 { width: 30px }` computes 30px until script reads `styleEl.sheet` and 40px
/// afterwards. Nothing could observe it before this unit, because a linked sheet had no rules to lose
/// the tie with.
///
/// `new_sheet` survives for the one sheet that has no element: `new CSSStyleSheet()`.
fn mint_element_sheet<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    el: v8::Local<'s, v8::Object>,
) -> Option<v8::Local<'s, v8::Object>> {
    let node = crate::binding::protos::unwrap_id(scope, el.into())?;
    let proto = shell_proto(scope, "CSSStyleSheet")?;
    let obj = crate::binding::protos::mint_on(scope, proto.into());
    let id = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        cssom::sheet_for_element(&mut st, node)
    };
    let idv: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, id).into();
    priv_set(scope, obj, SHEET_ID, idv);
    let owner: v8::Local<v8::Value> = el.into();
    priv_set(scope, obj, SHEET_OWNER, owner);
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<MintedSheets>().is_none() {
            st.put(MintedSheets::default());
        }
        st.borrow_mut::<MintedSheets>().0.insert(node);
    }
    let title = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<crate::binding::HostDom>().and_then(|h| h.0.get_attribute(node, "title").filter(|t| !t.is_empty()).map(str::to_string))
    };
    if let Some(t) = title {
        let v = str_val(scope, &t);
        priv_set(scope, obj, SHEET_OWNER_TITLE, v);
    }
    // A linked sheet's location is the URL its bytes came from, whatever the link's `href` says later: Chrome 152
    // answers the old URL while a new `href` loads, and the new sheet the new one.
    if let Some(url) = crate::core::html::subresource::link_sheet_url(scope, node) {
        let v = str_val(scope, &url);
        priv_set(scope, obj, SHEET_HREF, v);
    }
    // The text the pipeline's sheet was just synced from, so `resync_from_text` reparses only when
    // the text moves. With no record the first read of every `<style>`'s `.sheet` parsed the same
    // text again, and every reparse is a sheet edit that drops the document's matched rules and
    // style caches: www.salesforce.com reads `.sheet` off hundreds of component `<style>`s between
    // layouts, and each read cost a whole-document restyle.
    if let Some(text) = style_text(scope, el) {
        let tv = str_val(scope, &text);
        priv_set(scope, obj, SHEET_SRC, tv);
    }
    Some(obj)
}

/// A `<style>` element's text, `None` for any other element.
/// Its CHILD text content (HTML "update a style block": the element's child text content), read off the
/// arena like everything else here.
fn style_text(scope: &mut v8::PinScope, el: v8::Local<v8::Object>) -> Option<String> {
    if crate::page::local_name(scope, el.into()).as_deref() != Some("style") {
        return None;
    }
    Some(crate::page::child_text_content(scope, el.into()))
}

/// Re-parse a `<style>` element's sheet when its text content has changed since the last read.
fn resync_from_text(scope: &mut v8::PinScope, el: v8::Local<v8::Object>, sheet: v8::Local<v8::Object>) {
    let Some(text) = style_text(scope, el) else { return };
    let prev = priv_get(scope, sheet, SHEET_SRC).map(|v| v.to_rust_string_lossy(scope));
    if prev.as_deref() == Some(text.as_str()) {
        return;
    }
    let tv = str_val(scope, &text);
    priv_set(scope, sheet, SHEET_SRC, tv);
    let id = sheet_handle(scope, sheet);
    if id != NO_PIPELINE {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        cssom::sheet_replace(&mut st, id, &text);
    }
    invalidate_rules(scope, sheet);
    // The fresh parse has no imported sheets attached; the loads module re-attaches the ones that
    // have arrived.
    crate::core::css::sheet_loads::sheet_reparsed(scope);
}

// ── `StyleSheet` bodies ──────────────────────────────────────────────────────────────────────────

fn cb_ss_type(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !sheet_ok(scope, args.this()) { return }
    // CSSOM: "type" is always the string "text/css" for a CSS style sheet.
    let v = str_val(scope, "text/css");
    rv.set(v);
}

/// `href` — the sheet's location. CSSOM: the URL the sheet was fetched from, and null for a sheet
/// with no location, which is every `<style>` and every constructed sheet. A `<link>`'s is its
/// resolved `href`; the deleted glue answered null for that too.
fn cb_ss_href(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !sheet_ok(scope, args.this()) { return }
    let sheet = args.this();
    // Asked first: a sheet whose owner let it go is detached here, with the URL it had ([`detach_element_sheet`]).
    let owner = owner_of(scope, sheet);
    if let Some(h) = priv_get(scope, sheet, SHEET_HREF) {
        rv.set(h);
        return;
    }
    let Some(owner) = owner else { rv.set_null(); return };
    let local = crate::page::local_name(scope, owner.into()).unwrap_or_default();
    if local != "link" {
        rv.set_null();
        return;
    }
    match get_prop(scope, owner, "href").filter(|v| v.is_string()) {
        Some(h) if !h.to_rust_string_lossy(scope).is_empty() => rv.set(h),
        _ => rv.set_null(),
    }
}

fn cb_ss_owner_node(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !sheet_ok(scope, args.this()) { return }
    let sheet = args.this();
    match owner_of(scope, sheet) {
        Some(o) => rv.set(o.into()),
        None => rv.set_null(),
    }
}

/// `parentStyleSheet` — the sheet that `@import`-ed this one, null for a top-level sheet.
fn cb_ss_parent(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !sheet_ok(scope, args.this()) { return }
    match priv_get(scope, args.this(), SHEET_PARENT) {
        Some(p) => rv.set(p),
        None => rv.set_null(),
    }
}

/// `title` — CSSOM: the owner node's `title` content attribute, and null when it is absent or empty.
fn cb_ss_title(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !sheet_ok(scope, args.this()) { return }
    let sheet = args.this();
    // A DETACHED sheet answers the title its owner had ([`detach_element_sheet`]) -- detached by this read if its
    // owner has let it go.
    let owned = owner_of(scope, sheet).is_some();
    if let Some(t) = priv_get(scope, sheet, SHEET_TITLE) {
        rv.set(t);
        return;
    }
    // A CONSTRUCTED sheet answers null whatever its init said -- see `cb_sheet_ctor`.
    match owner_attr(scope, sheet, "title").filter(|_| owned) {
        Some(t) if !t.is_empty() => {
            let v = str_val(scope, &t);
            rv.set(v);
        }
        _ => rv.set_null(),
    }
}

/// A sheet's disabled flag (`<style>.disabled` reads and writes it through its sheet).
pub(crate) fn sheet_disabled(scope: &mut v8::PinScope, sheet: v8::Local<v8::Object>) -> bool {
    priv_get(scope, sheet, SHEET_DISABLED).map(|v| v.boolean_value(scope)).unwrap_or(false)
}
/// `StyleSheet.disabled = b`: a disabled sheet styles nothing while its rules stay readable and writable. Chrome
/// 152.0.7977.82, 2026-10-23:
///
/// ```text
///   <style>.sheet.disabled = true (styleSheets[i] too)   unstyled at once; listed; cssRules readable
///   an insertRule while disabled, then enabled           styled, the inserted rule with it
///   the <style>'s text changed, or the element moved     a new sheet, enabled, styling; the old one disabled
///   a <link>'s sheet, then the link moved                unstyled; the moved link's new sheet styles
///   an adopted constructed sheet, {disabled: true}       unstyled, and styled again when enabled
/// ```
///
/// This engine kept the flag for script to read back and styled with the sheet all the same -- a theme toggle through
/// `document.styleSheets[i].disabled` did nothing. An element's sheet cannot just switch its row in the cascade off:
/// the sheet sync owns that row's switch (on while the element is in its document) and turns it back on at its next
/// walk. So its rules MOVE to a page handle of the sheet's own, switched off ([`give_copy`]), the row stays empty
/// ([`SHEET_HOME`]) until the sheet is enabled and they move back -- or its element moves, and the row is the next
/// sheet's ([`detach_element_sheet`]). A `<style>` text change is parsed into the row by the sheet sync, so it styles
/// as Chrome's new sheet does (this engine keeps the sheet object, disabled, where Chrome makes a new one). A
/// constructed sheet's handle is its own already.
pub(crate) fn set_sheet_disabled(scope: &mut v8::PinScope, sheet: v8::Local<v8::Object>, b: bool) {
    let was = sheet_disabled(scope, sheet);
    let v: v8::Local<v8::Value> = v8::Boolean::new(scope, b).into();
    priv_set(scope, sheet, SHEET_DISABLED, v);
    // An imported sheet's rules are its parent's, which its flag does not switch.
    if was == b || priv_get(scope, sheet, SHEET_PREFIX).is_some() {
        return;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let Some(p) = page_handle(&op_state.borrow()) else { return };
    if priv_get(scope, sheet, SHEET_CONSTRUCTED).is_some() {
        let h = sheet_handle(scope, sheet);
        if h != NO_PIPELINE {
            p.with_sheet_mut(h, |s| s.disabled = b);
        }
        return;
    }
    // A sheet its element has let go of styles nothing already (detached by the asking).
    let Some(owner) = owner_of(scope, sheet) else { return };
    let home = priv_get(scope, sheet, SHEET_HOME).and_then(|v| v.uint32_value(scope));
    let id = priv_get(scope, sheet, SHEET_ID).and_then(|v| v.uint32_value(scope)).unwrap_or(NO_PIPELINE);
    if id == NO_PIPELINE || id & DETACHED != 0 {
        return;
    }
    match (b, home) {
        (true, None) => {
            let Some(mut rules) = p.with_sheet_mut(id, |s| {
                let mut row = emptied();
                row.disabled = s.disabled;
                row.origin_clean = s.origin_clean;
                row.base_url = s.base_url.clone();
                let media = s.media_text().to_string();
                row.set_media_text(&media);
                std::mem::replace(s, row)
            }) else {
                return;
            };
            rules.disabled = true;
            let copy = give_copy(scope, sheet, &p, rules);
            let c: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, copy).into();
            priv_set(scope, sheet, SHEET_ID, c);
            let h: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, id).into();
            priv_set(scope, sheet, SHEET_HOME, h);
        }
        (false, Some(home)) => {
            // The row as the sheet sync has it now: a `<style>` whose text moved since its sheet last read it had that
            // text parsed into the row, and those are its rules; else they are the ones the sheet holds.
            if let Some(el) = crate::binding::protos::unwrap_id(scope, owner.into()) {
                cssom::sheet_for_element(&mut op_state.borrow_mut(), el);
            }
            let text = style_text(scope, owner);
            let read = priv_get(scope, sheet, SHEET_SRC).map(|v| v.to_rust_string_lossy(scope));
            match text {
                Some(t) if read.as_deref() != Some(t.as_str()) => {
                    let tv = str_val(scope, &t);
                    priv_set(scope, sheet, SHEET_SRC, tv);
                    invalidate_rules(scope, sheet);
                }
                _ => {
                    if let Some(mut rules) = p.with_sheet_mut(id, |s| std::mem::replace(s, emptied())) {
                        p.with_sheet_mut(home, move |s| {
                            rules.disabled = s.disabled;
                            *s = rules;
                        });
                    }
                }
            }
            let h: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, home).into();
            priv_set(scope, sheet, SHEET_ID, h);
            let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
            priv_set(scope, sheet, SHEET_HOME, undef);
            let mut st = op_state.borrow_mut();
            touch(&mut st, home);
            release_copy(&mut st, &p, id);
        }
        _ => {}
    }
}

fn cb_ss_disabled_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !sheet_ok(scope, args.this()) { return }
    let sheet = args.this();
    let b = priv_get(scope, sheet, SHEET_DISABLED).map(|v| v.boolean_value(scope)).unwrap_or(false);
    rv.set_bool(b);
}
fn cb_ss_disabled_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !sheet_ok(scope, args.this()) { return }
    let sheet = args.this();
    let b = args.get(0).boolean_value(scope);
    set_sheet_disabled(scope, sheet, b);
}

/// `media` — the `[SameObject]` `MediaList`, minted once per sheet.
fn cb_ss_media_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !sheet_ok(scope, args.this()) { return }
    let sheet = args.this();
    if let Some(m) = priv_get(scope, sheet, SHEET_MEDIA).filter(|v| v.is_object()) {
        rv.set(m);
        return;
    }
    let Some(proto) = shell_proto(scope, "MediaList") else { return };
    let m = crate::binding::protos::mint_on(scope, proto.into());
    priv_set(scope, m, MEDIA_SHEET, sheet.into());
    priv_set(scope, sheet, SHEET_MEDIA, m.into());
    rv.set(m.into());
}
/// `media`'s setter is `[PutForwards=mediaText]`: `sheet.media = "print"` sets the media text.
fn cb_ss_media_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !sheet_ok(scope, args.this()) { return }
    let sheet = args.this();
    let text = args.get(0).to_rust_string_lossy(scope);
    set_media_text(scope, sheet, &text);
}

// ── the media list: the SHEET's own ────────────────────────────────────────────────
//
// It used to read and write the owner element's `media` ATTRIBUTE, and nothing in the cascade read
// either, so `<style media=print>` reported `print` and applied to the screen anyway (L19 E5). The list
// lives on the `korvus-page` sheet now -- the cascade's own `Stylesheet::sheet_applies` reads it -- and
// the attribute only initialises it (`Page::sync_document_sheets`). An imported sheet's list is its
// `@import` rule's condition, which the cascade already honours; it keeps the private text.

/// The `korvus-page` handle whose media list this sheet IS, or `None` for an imported sheet / no
/// pipeline.
fn media_handle(scope: &mut v8::PinScope, sheet: v8::Local<v8::Object>) -> Option<u32> {
    let handle = sheet_handle(scope, sheet);
    (handle != NO_PIPELINE && sheet_prefix(scope, sheet).is_empty()).then_some(handle)
}

fn media_text(scope: &mut v8::PinScope, sheet: v8::Local<v8::Object>) -> String {
    if let Some(handle) = media_handle(scope, sheet) {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let text = cssom::sheet_media_text(&mut op_state.borrow_mut(), handle);
        if let Some(t) = text {
            return t;
        }
    }
    priv_get(scope, sheet, SHEET_MEDIA_TEXT).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default()
}
fn set_media_text(scope: &mut v8::PinScope, sheet: v8::Local<v8::Object>, text: &str) {
    if let Some(handle) = media_handle(scope, sheet) {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let done = cssom::set_sheet_media_text(&mut op_state.borrow_mut(), handle, text);
        if done {
            return;
        }
    }
    let v = str_val(scope, text);
    priv_set(scope, sheet, SHEET_MEDIA_TEXT, v);
}
/// The media query list as its items: each query as Blink parses and serializes it (`page::style::media`,
/// `MediaQuerySet::QueryVector`). Measured on stock Chrome 150.0.7871.46:
///
/// ```text
/// "SCREEN"                          -> "screen"
/// "Screen And (MIN-WIDTH:10PX)"     -> "screen and (min-width: 10px)"
/// "(min-width:  100px)"             -> "(min-width: 100px)"
/// "  print  "                       -> "print"
/// "garbage((", "foo", "(bogus: 1)"  -> kept as written
/// ```
///
/// It split the text at every comma and normalized each piece's text -- a `(feature:value)` always got its
/// space -- where an unknown feature is kept exactly as written: an `@media (color) or (foo:bar)` rule's
/// `media.mediaText` is `(color) or (foo:bar)` in Chrome 150 (`fixtures/css-answers`).
fn media_items(scope: &mut v8::PinScope, sheet: v8::Local<v8::Object>) -> Vec<String> {
    page::style::media::serialize_each(&media_text(scope, sheet))
}

/// The `MediaList`'s sheet, or `None` with `Illegal invocation` thrown.
fn media_recv<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<'_, v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    match priv_get(scope, this, MEDIA_SHEET).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        Some(s) => Some(s),
        None => {
            throw_type_error(scope, "Illegal invocation");
            None
        }
    }
}

fn cb_ml_length(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(sheet) = media_recv(scope, args.this()) else { return };
    let n = media_items(scope, sheet).len();
    rv.set_uint32(n as u32);
}
fn cb_ml_text_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(sheet) = media_recv(scope, args.this()) else { return };
    let t = media_items(scope, sheet).join(", ");
    let v = str_val(scope, &t);
    rv.set(v);
}
fn cb_ml_text_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(sheet) = media_recv(scope, args.this()) else { return };
    let t = args.get(0).to_rust_string_lossy(scope);
    set_media_text(scope, sheet, &t);
}
fn cb_ml_item(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(sheet) = media_recv(scope, args.this()) else { return };
    let i = args.get(0).uint32_value(scope).unwrap_or(0) as usize;
    match media_items(scope, sheet).get(i) {
        Some(s) => {
            let v = str_val(scope, s);
            rv.set(v);
        }
        None => rv.set_null(),
    }
}
/// `MediaList`'s indexed properties: `media[0]` is `item(0)`, `0 in media` is true and the
/// indices are its own keys -- Brave 151: `[...sheet.media]` is `["screen", "print"]`. Only `item()` answered,
/// so `media[0]` read `undefined` and the list's value iterator (`Array.prototype.values`, which reads the
/// indices) walked `length` holes.
pub(crate) fn configure_media_list(it: v8::Local<v8::ObjectTemplate>) {
    it.set_indexed_property_handler(
        v8::IndexedPropertyHandlerConfiguration::new()
            .getter_raw(crate::binding::refs::c(icpt!(idx_get, "MediaList", cb_ml_idx_get).map_fn_to()))
            .query_raw(crate::binding::refs::c(icpt!(idx_query, "MediaList", cb_ml_idx_query).map_fn_to()))
            .enumerator_raw(crate::binding::refs::c(icpt!(idx_keys, "MediaList", cb_ml_idx_enum).map_fn_to())),
    );
}

/// The media items of the `MediaList` `holder` is, without a throw for anything else.
fn holder_items(scope: &mut v8::PinScope, holder: v8::Local<v8::Object>) -> Option<Vec<String>> {
    let sheet = priv_get(scope, holder, MEDIA_SHEET).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    Some(media_items(scope, sheet))
}

fn cb_ml_idx_get<'s>(scope: &mut v8::PinScope<'s, '_>, index: u32, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue) -> v8::Intercepted {
    let Some(items) = holder_items(scope, args.holder()) else { return v8::Intercepted::kNo };
    match items.get(index as usize) {
        Some(t) => {
            let v = str_val(scope, t);
            rv.set(v);
            v8::Intercepted::kYes
        }
        None => v8::Intercepted::kNo,
    }
}

fn cb_ml_idx_query<'s>(scope: &mut v8::PinScope<'s, '_>, index: u32, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue<v8::Integer>) -> v8::Intercepted {
    let Some(items) = holder_items(scope, args.holder()) else { return v8::Intercepted::kNo };
    if (index as usize) >= items.len() {
        return v8::Intercepted::kNo;
    }
    // {writable:false, enumerable:true, configurable:true} -- WebIDL's indexed property.
    let attrs = v8::Integer::new(scope, v8::PropertyAttribute::READ_ONLY.as_u32() as i32);
    rv.set(attrs);
    v8::Intercepted::kYes
}

fn cb_ml_idx_enum<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue<v8::Array>) {
    let n = holder_items(scope, args.holder()).map_or(0, |i| i.len());
    let arr = v8::Array::new(scope, n as i32);
    for i in 0..n {
        let k = v8::Integer::new_from_unsigned(scope, i as u32);
        arr.set_index(scope, i as u32, k.into());
    }
    rv.set(arr);
}

fn cb_ml_to_string(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, rv: v8::ReturnValue) {
    cb_ml_text_get(scope, args, rv)
}
fn cb_ml_append(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(sheet) = media_recv(scope, args.this()) else { return };
    let m = args.get(0).to_rust_string_lossy(scope).trim().to_string();
    if m.is_empty() {
        return;
    }
    let mut items = media_items(scope, sheet);
    // CSSOM: appending a medium already in the list is a no-op (the comparison is case-insensitive).
    if items.iter().any(|x| x.eq_ignore_ascii_case(&m)) {
        return;
    }
    items.push(m);
    let joined = items.join(", ");
    set_media_text(scope, sheet, &joined);
}
fn cb_ml_delete(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(sheet) = media_recv(scope, args.this()) else { return };
    let m = args.get(0).to_rust_string_lossy(scope).trim().to_string();
    let items = media_items(scope, sheet);
    let kept: Vec<String> = items.into_iter().filter(|x| !x.eq_ignore_ascii_case(&m)).collect();
    let joined = kept.join(", ");
    set_media_text(scope, sheet, &joined);
}

// ── the CSSOM's own exceptions ───────────────────────────────────────────────────────────────────
//
// EVERY ONE OF THESE WAS A `TypeError` BEFORE THIS UNIT, and CSSOM says `IndexSizeError` /
// `SyntaxError` -- a `DOMException`, which is a different constructor, a different `name` and a
// different `code`, so a page's `catch (e) { if (e.name === "IndexSizeError") … }` took the wrong
// branch. Asked of `google-chrome --headless=new` 141 on 2026-09-12, on a one-rule `<style>` sheet:
//
// ```text
// s.insertRule("b{}", 99)  DOMException/IndexSizeError: Failed to execute 'insertRule' on
//                          'CSSStyleSheet': The index provided (99) is larger than the maximum index (1).
// s.insertRule("not css")  DOMException/SyntaxError: … Failed to parse the rule 'not css'.
// s.insertRule("b{}", -1)  DOMException/IndexSizeError: … The index provided (4294967295) …
// s.deleteRule(99)         DOMException/IndexSizeError: … larger than the maximum index (0).
// c.removeRule()           DOMException/IndexSizeError: … Style sheet is empty (length 0).
// s.insertRule()           TypeError: … 1 argument required, but only 0 present.
// ```
//
// The messages are reproduced exactly, including the numbers -- a page that prints `e.message` sees
// the same string -- and the last line is the ROW's `required 1` arity guard, which stays a TypeError.

/// WebIDL `unsigned long`: `ToUint32`, so `-1` is 4294967295 and lands in the out-of-range branch
/// with Chrome's own number in the message. The deleted glue used `index | 0` and then tested `< 0`,
/// which reported `-1` as its own kind of failure.
fn idl_index(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> u32 {
    v.uint32_value(scope).unwrap_or(0)
}

/// "The index provided (N) is larger than the maximum index (M)." -- or, for an empty sheet, the
/// wording Chrome uses instead.
fn throw_index_size(scope: &mut v8::PinScope, member: &str, index: u32, max: Option<u32>) {
    let msg = match max {
        Some(m) => format!(
            "Failed to execute '{member}' on 'CSSStyleSheet': The index provided ({index}) is larger than the maximum index ({m})."
        ),
        None => format!("Failed to execute '{member}' on 'CSSStyleSheet': Style sheet is empty (length 0)."),
    };
    throw_dom(scope, &msg, "IndexSizeError");
}

// ── `CSSStyleSheet` bodies ───────────────────────────────────────────────────────────────────────

/// The rule WRAPPERS for this sheet, cached so `sheet.cssRules[0] === sheet.cssRules[0]` holds and
/// rebuilt whenever the rule count has moved.
fn rules_of<'s>(scope: &mut v8::PinScope<'s, '_>, sheet: v8::Local<'s, v8::Object>) -> v8::Local<'s, v8::Array> {
    let n = sheet_rule_count(scope, sheet);
    if let Some(a) = priv_get(scope, sheet, SHEET_RULES).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
        if a.length() == n {
            return a;
        }
    }
    let prefix = sheet_prefix(scope, sheet);
    let a = v8::Array::new(scope, n as i32);
    for i in 0..n {
        let mut path = prefix.clone();
        path.push(i);
        if let Some(r) = crate::core::css::css_rule::make_rule(scope, sheet, &path, None) {
            a.set_index(scope, i, r.into());
        }
    }
    priv_set(scope, sheet, SHEET_RULES, a.into());
    a
}

/// `cssRules` (and its legacy alias `rules`) — a `CSSRuleList`, not an Array.
///
/// The deleted glue answered `rulesOf(sheet).slice()` with an `item` closure stapled on, so
/// `Array.isArray(sheet.cssRules)` was TRUE (false in every browser), `instanceof CSSRuleList` was
/// false, and a fresh Array came back on every read. `static_list_reuse` refills ONE native list in
/// place, so `sheet.cssRules === sheet.cssRules` — which is also what a browser answers.
/// The two spellings are two callbacks for ONE reason: the DOMException a non-origin-clean sheet
/// throws names the member, and Chrome's two messages differ by exactly that word (measured
/// 2026-09-13 — `Failed to read the 'rules' property …` against `… the 'cssRules' property …`).
fn css_rules_named<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, rv: &mut v8::ReturnValue, member: &str) {
    if !sheet_ok(scope, args.this()) { return }
    let sheet = args.this();
    let msg = format!("Failed to read the '{member}' property from 'CSSStyleSheet': Cannot access rules");
    if !rules_readable(scope, sheet, &msg) {
        return;
    }
    let arr = rules_of(scope, sheet);
    let existing = priv_get(scope, sheet, SHEET_RULELIST).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    let list = static_list_reuse(scope, "CSSRuleList", arr, existing);
    priv_set(scope, sheet, SHEET_RULELIST, list);
    rv.set(list);
}
fn cb_sheet_css_rules<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    css_rules_named(scope, args, &mut rv, "cssRules")
}
fn cb_sheet_rules<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    css_rules_named(scope, args, &mut rv, "rules")
}

/// `insertRule(rule, index)`.
///
/// THE DEFAULT INDEX IS 0, NOT THE RULE COUNT. The deleted glue APPENDED when `index` was omitted,
/// and `core/korvus-runtime/tests/cssom.rs` recorded that as the engine's answer. CSSOM says
/// `optional unsigned long index = 0`, and headless Chrome 141 agrees, measured 2026-09-12 on a
/// constructed sheet holding one rule:
///
/// ```text
/// c.insertRule("b{color:blue}")   ->  0      c.cssRules[0].selectorText  ->  "b"
/// ```
///
/// (`addRule`, the legacy member below, DOES default to the rule count -- also measured, also what
/// CSSOM says. The two differ, which is why the old glue's single "append" rule was wrong for one of
/// them.) The disagreeing case in `tests/cssom.rs` is corrected against that measurement rather than
/// preserved: truth source 2 proves the engine wrong, so the engine moves.
fn cb_sheet_insert_rule(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !sheet_ok(scope, args.this()) { return }
    let sheet = args.this();
    if !rules_readable(scope, sheet, "Failed to execute 'insertRule' on 'CSSStyleSheet': Cannot access StyleSheet to insertRule") {
        return;
    }
    let n = sheet_rule_count(scope, sheet);
    let text = args.get(0).to_rust_string_lossy(scope);
    let i = if args.get(1).is_undefined() { 0 } else { idl_index(scope, args.get(1)) };
    if i > n {
        throw_index_size(scope, "insertRule", i, Some(n));
        return;
    }
    let id = sheet_handle(scope, sheet);
    let prefix = sheet_prefix(scope, sheet);
    // A CONSTRUCTED sheet takes no `@import` -- Chrome 150: "SyntaxError: Failed to execute
    // 'insertRule' on 'CSSStyleSheet': Can't insert @import rules into a constructed stylesheet."
    if priv_get(scope, sheet, SHEET_CONSTRUCTED).is_some() && text.trim_start().get(..7).is_some_and(|h| h.eq_ignore_ascii_case("@import")) {
        throw_dom(scope, "Failed to execute 'insertRule' on 'CSSStyleSheet': Can't insert @import rules into a constructed stylesheet.", "SyntaxError");
        return;
    }
    let r = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        cssom::insert_rule_at(&mut st, id, &prefix, &text, i)
    };
    if r == cssom::ERR_HIERARCHY {
        throw_dom(scope, "Failed to execute 'insertRule' on 'CSSStyleSheet': Failed to insert the rule.", "HierarchyRequestError");
        return;
    }
    // -1 index, -2 syntax, -3 no pipeline. Only the syntax case is a page-visible difference from the
    // bounds check just above, which the parser can disagree with about `@import`.
    if r == -2 {
        let msg = format!("Failed to execute 'insertRule' on 'CSSStyleSheet': Failed to parse the rule '{text}'.");
        throw_dom(scope, &msg, "SyntaxError");
        return;
    }
    if r < 0 {
        throw_index_size(scope, "insertRule", i, Some(n));
        return;
    }
    if !append_wrapper(scope, sheet, &prefix, n, r as u32) {
        invalidate_rules(scope, sheet);
    }
    rv.set_int32(r);
}

/// After `insertRule` put a rule at `at` of a sheet that held `n`: when that was an APPEND and every wrapper is made,
/// the new rule's wrapper joins them and nothing is made again. An append shifts no index, so each wrapper still names
/// its rule -- and stays the same object, as Chrome's do (`r = sheet.cssRules[0]` is `sheet.cssRules[0]` after an
/// append). A CSS-in-JS runtime appends each rule it makes (`insertRule(rule, sheet.cssRules.length)`), and throwing
/// every wrapper away on each made the next `cssRules` read make all of them again: 4.8 % of www.costco.com's main
/// thread up to a bot-detection endpoint's `/tl` (2026-10-22). `false` -- anything else -- and the caller throws the wrappers away.
fn append_wrapper(scope: &mut v8::PinScope, sheet: v8::Local<v8::Object>, prefix: &[u32], n: u32, at: u32) -> bool {
    if at != n {
        return false;
    }
    let sheet = v8::Local::new(scope, sheet);
    let Some(made) = priv_get(scope, sheet, SHEET_RULES).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) else { return false };
    if made.length() != n || sheet_rule_count(scope, sheet) != n + 1 {
        return false;
    }
    let mut path = prefix.to_vec();
    path.push(n);
    let Some(rule) = crate::core::css::css_rule::make_rule(scope, sheet, &path, None) else { return false };
    made.set_index(scope, n, rule.into()).unwrap_or(false)
}

fn delete_rule_named(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, member: &str) {
    if !sheet_ok(scope, args.this()) { return }
    let sheet = args.this();
    let msg = format!("Failed to execute '{member}' on 'CSSStyleSheet': Cannot access StyleSheet to deleteRule");
    if !rules_readable(scope, sheet, &msg) {
        return;
    }
    let n = sheet_rule_count(scope, sheet);
    let i = idl_index(scope, args.get(0));
    if i >= n {
        throw_index_size(scope, member, i, n.checked_sub(1));
        return;
    }
    let id = sheet_handle(scope, sheet);
    let prefix = sheet_prefix(scope, sheet);
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        cssom::delete_rule_at(&mut st, id, &prefix, i);
    }
    invalidate_rules(scope, sheet);
}
fn cb_sheet_delete_rule(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    delete_rule_named(scope, &args, "deleteRule")
}

/// `addRule(selector, style, index)` — CSSOM's legacy member, spelled exactly as CSSOM defines it:
/// build `"<selector> { <style> }"`, insert it at `index` (default: the rule count), return `-1`.
/// Both arguments default to the STRING `"undefined"`, which is the spec's own wording. It was a
/// registry `stub` (answering `undefined`) before this table.
fn cb_sheet_add_rule(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !sheet_ok(scope, args.this()) { return }
    let sheet = args.this();
    if !rules_readable(scope, sheet, "Failed to execute 'addRule' on 'CSSStyleSheet': Cannot access StyleSheet to insertRule") {
        return;
    }
    let selector = args.get(0).to_rust_string_lossy(scope);
    let block = args.get(1).to_rust_string_lossy(scope);
    let n = sheet_rule_count(scope, sheet);
    let i = if args.get(2).is_undefined() { n } else { idl_index(scope, args.get(2)) };
    if i > n {
        throw_index_size(scope, "addRule", i, Some(n));
        return;
    }
    let rule = if block.is_empty() { format!("{selector} {{ }}") } else { format!("{selector} {{ {block} }}") };
    let id = sheet_handle(scope, sheet);
    let prefix = sheet_prefix(scope, sheet);
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        cssom::insert_rule_at(&mut st, id, &prefix, &rule, i);
    }
    invalidate_rules(scope, sheet);
    // CSSOM: "Return -1." Unconditionally -- addRule does not report where the rule landed.
    rv.set_int32(-1);
}

/// `removeRule(index)` — CSSOM: "run the same steps as deleteRule()", with `index` defaulting to 0.
fn cb_sheet_remove_rule(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    delete_rule_named(scope, &args, "removeRule")
}

fn replace_sync_inner(scope: &mut v8::PinScope, sheet: v8::Local<v8::Object>, text: &str) {
    let id = sheet_handle(scope, sheet);
    if id != NO_PIPELINE {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        cssom::sheet_replace(&mut st, id, text);
    }
    invalidate_rules(scope, sheet);
}

/// Is this a CONSTRUCTED sheet? `replaceSync` and `replace` are allowed on nothing else.
///
/// Measured in headless Chrome 141, 2026-09-12, on a `<style>` element's sheet:
/// `s.replaceSync("z{color:pink}")` throws `DOMException/NotAllowedError: Failed to execute
/// 'replaceSync' on 'CSSStyleSheet': Can't call replaceSync on non-constructed CSSStyleSheets.`
/// This engine let it through, and `tests/cssom.rs` recorded that -- corrected against the
/// measurement in the same commit. A `<style>`'s sheet is still re-parsed from its text; that path is
/// `resync_from_text`, which calls the pipeline directly and never goes through this member.
fn constructed_only(scope: &mut v8::PinScope, sheet: v8::Local<v8::Object>, member: &str) -> bool {
    if priv_get(scope, sheet, SHEET_CONSTRUCTED).is_some() {
        return true;
    }
    let msg = format!(
        "Failed to execute '{member}' on 'CSSStyleSheet': Can't call {member} on non-constructed CSSStyleSheets."
    );
    throw_dom(scope, &msg, "NotAllowedError");
    false
}

fn cb_sheet_replace_sync(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !sheet_ok(scope, args.this()) { return }
    let sheet = args.this();
    if !constructed_only(scope, sheet, "replaceSync") { return }
    let text = args.get(0).to_rust_string_lossy(scope);
    replace_sync_inner(scope, sheet, &text);
}

/// `replace(text)` — the async form: it resolves with the sheet itself once the text is parsed. There
/// is nothing to await here (the parser is synchronous), so it is `replaceSync` in a resolved promise,
/// which is exactly what the deleted glue did.
fn cb_sheet_replace(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !sheet_ok(scope, args.this()) { return }
    let sheet = args.this();
    let text = args.get(0).to_rust_string_lossy(scope);
    let Some(resolver) = v8::PromiseResolver::new(scope) else { return };
    let promise = resolver.get_promise(scope);
    // `replace` REJECTS where `replaceSync` throws -- the async form of the same refusal. The
    // `NotAllowedError` is built by the same helper and then CAUGHT, so there is one message.
    if priv_get(scope, sheet, SHEET_CONSTRUCTED).is_some() {
        replace_sync_inner(scope, sheet, &text);
        resolver.resolve(scope, sheet.into());
    } else {
        let err: Option<v8::Global<v8::Value>> = {
            v8::tc_scope!(let tc, scope);
            constructed_only(tc, sheet, "replace");
            let e = tc.exception().map(|e| v8::Global::new(tc, e));
            tc.reset();
            e
        };
        match err {
            Some(g) => {
                let e = v8::Local::new(scope, &g);
                resolver.reject(scope, e);
            }
            None => {
                resolver.reject(scope, sheet.into());
            }
        }
    }
    rv.set(promise.into());
}

/// `ownerRule` — the `@import` rule that pulled this sheet in, null for a sheet that was not imported.
fn cb_sheet_owner_rule(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !sheet_ok(scope, args.this()) { return }
    match priv_get(scope, args.this(), SHEET_OWNER_RULE) {
        Some(r) => rv.set(r),
        None => rv.set_null(),
    }
}

/// `new CSSStyleSheet(options)` — a CONSTRUCTED style sheet, owned by no element.
///
/// `options` is a `CSSStyleSheetInit`: `media` (a string), `title`, `disabled` and `baseURL`.
///
/// `title` IS ACCEPTED AND NOT REPORTED BACK, because that is what Chrome does. Measured in headless
/// Chrome 141, 2026-09-12: `new CSSStyleSheet({media: "screen", title: "c", disabled: true})` answers
/// `media.mediaText === "screen"`, `disabled === true` and **`title === null`**. `baseURL` is likewise
/// accepted and ignored -- there is no `@import` here to resolve against.
pub(crate) fn cb_sheet_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // THE `new` CHECK COMES FIRST: a bare `CSSStyleSheet()` used to build and RETURN a
    // real constructed sheet where Chrome throws.
    if !crate::binding::protos::require_ctor_new_target(scope, &args, "CSSStyleSheet") {
        return;
    }
    // `CSSStyleSheetInit`, converted as WebIDL converts it (Chrome 150): `alternate`, `disabled`,
    // `media`, each read once, before the sheet exists; a non-object is no dictionary (korvus read `media` then
    // `disabled`, and took a number for none)
    let Ok(init) = crate::install::ctor_dicts::convert_dict(scope, args.get(0), "CSSStyleSheetInit", "Failed to construct 'CSSStyleSheet': ") else { return };
    let Some(sheet) = new_sheet(scope, None) else { return };
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    priv_set(scope, sheet, SHEET_CONSTRUCTED, t);
    if let Some(m) = get_prop(scope, init, "media").filter(|v| !v.is_undefined()) {
        let t = m.to_rust_string_lossy(scope);
        set_media_text(scope, sheet, &t);
    }
    if let Some(d) = get_prop(scope, init, "disabled").filter(|v| !v.is_undefined()) {
        // adopted, it styles nothing ([`set_sheet_disabled`])
        set_sheet_disabled(scope, sheet, d.is_true());
    }
    rv.set(sheet.into());
}

// ── the borrowed `SVGStyleElement.sheet` ─────────────────────────────────────────────────────────


// ── the row installers ───────────────────────────────────────────────────────────────────────────

/// The body every row of [`STYLESHEET`] names.
pub fn install_style_sheet(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::install::live::{getter_row, pair_row};
    match member.name {
        "type" => getter_row(scope, proto, member, cb_ss_type),
        "href" => getter_row(scope, proto, member, cb_ss_href),
        "ownerNode" => getter_row(scope, proto, member, cb_ss_owner_node),
        "parentStyleSheet" => getter_row(scope, proto, member, cb_ss_parent),
        "title" => getter_row(scope, proto, member, cb_ss_title),
        "media" => pair_row(scope, proto, member, cb_ss_media_get, cb_ss_media_set),
        "disabled" => pair_row(scope, proto, member, cb_ss_disabled_get, cb_ss_disabled_set),
        other => debug_assert!(false, "install_style_sheet: unexpected member {other}"),
    }
}

/// The body every row of [`CSSSTYLESHEET`] names.
pub fn install_css_style_sheet(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::install::live::{getter_row, method_row};
    match member.name {
        "ownerRule" => getter_row(scope, proto, member, cb_sheet_owner_rule),
        "cssRules" => getter_row(scope, proto, member, cb_sheet_css_rules),
        "rules" => getter_row(scope, proto, member, cb_sheet_rules),
        "addRule" => method_row(scope, proto, member, cb_sheet_add_rule),
        "deleteRule" => method_row(scope, proto, member, cb_sheet_delete_rule),
        "insertRule" => method_row(scope, proto, member, cb_sheet_insert_rule),
        "removeRule" => method_row(scope, proto, member, cb_sheet_remove_rule),
        "replace" => method_row(scope, proto, member, cb_sheet_replace),
        "replaceSync" => method_row(scope, proto, member, cb_sheet_replace_sync),
        other => debug_assert!(false, "install_css_style_sheet: unexpected member {other}"),
    }
}

/// The body every row of [`MEDIALIST`] names.
pub fn install_media_list(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::install::live::{getter_row, method_row, pair_row};
    match member.name {
        "length" => getter_row(scope, proto, member, cb_ml_length),
        "mediaText" => pair_row(scope, proto, member, cb_ml_text_get, cb_ml_text_set),
        "appendMedium" => method_row(scope, proto, member, cb_ml_append),
        "deleteMedium" => method_row(scope, proto, member, cb_ml_delete),
        "item" => method_row(scope, proto, member, cb_ml_item),
        "toString" => method_row(scope, proto, member, cb_ml_to_string),
        other => debug_assert!(false, "install_media_list: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `StyleSheet`, `CSSStyleSheet` and `MediaList` blocks of
// `install/generated_registry.rs`, in the capture's order.
webapi::interface! {
    "StyleSheet" [interface, window] {
        ctor "StyleSheet"                                   len 0 = stub,
        attr "type"             : DomString readonly              = install_style_sheet,
        attr "href"             : UsvString readonly nullable     = install_style_sheet,
        attr "ownerNode"        : Any       readonly nullable     = install_style_sheet,
        attr "parentStyleSheet" : Any       readonly nullable     = install_style_sheet,
        attr "title"            : DomString readonly nullable     = install_style_sheet,
        attr "media"            : Any                             = install_style_sheet,
        attr "disabled"         : Boolean                         = install_style_sheet
    }
}

webapi::interface! {
    "CSSStyleSheet" : "StyleSheet" [value, window] {
        ctor   "CSSStyleSheet"                      len 0 = stub,
        attr   "ownerRule"   : Any       readonly nullable = install_css_style_sheet,
        attr   "cssRules"    : Any       readonly          = install_css_style_sheet,
        attr   "rules"       : Any       readonly          = install_css_style_sheet,
        method "addRule"     : Integer   len 0             = install_css_style_sheet,
        method "deleteRule"  : Undefined len 1 required 1  = install_css_style_sheet,
        method "insertRule"  : Integer   len 1 required 1  = install_css_style_sheet,
        method "removeRule"  : Undefined len 0             = install_css_style_sheet,
        method "replace"     : Promise   len 1 required 1  = install_css_style_sheet,
        method "replaceSync" : Undefined len 1 required 1  = install_css_style_sheet
    }
}

webapi::interface! {
    "MediaList" [interface, window] {
        ctor   "MediaList"                              len 0 = stub,
        attr   "length"       : Integer   readonly            = install_media_list,
        attr   "mediaText"    : Any                           = install_media_list,
        method "appendMedium" : Undefined len 1 required 1     = install_media_list,
        method "deleteMedium" : Undefined len 1 required 1     = install_media_list,
        method "item"         : DomString len 1 required 1 nullable = install_media_list,
        method "toString"     : Any       len 0               = install_media_list,
        method "@@iterator" : Object len 0 hidden cfg "values" = array_proto_fn
    }
}
