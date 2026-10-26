//! `core/html/html_input_element.rs` — the `<input>` element (HTML §4.10.5) and the `ValidityState`
//! it hands out.
//!
//! `HTMLInputElement` is the widest interface in HTML: 61 WebIDL members, most of which mean something
//! DIFFERENT depending on the element's *type*. `input.height` is a reflected dimension on
//! `type=image` and hard 0 on everything else; `valueAsNumber` is a number on `type=number` and an
//! `InvalidStateError` on `type=text`; `selectionStart` is a caret offset on `type=text` and `null` on
//! `type=checkbox`. So the file is organized around one function — [`input_type`], the element's
//! *type* after the IDL's own "limited to only known values" clamp — and a handful of predicates over
//! it. Nothing here branches on a hardcoded expectation; every member asks the type what applies.
//!
//! What is NOT implemented, and why (each is a missing SUBSYSTEM, not a missing line):
//!
//!   * `files` on `type=file` is an empty `FileList` and stays empty. Populating it needs a file
//!     picker and a filesystem-backed `File`; Korvus has neither, and there is no honest way for a
//!     headless engine to have picked a file. The FileList object itself is real, so `files.length`,
//!     `files.item(0)` and iteration all work — they just describe an empty selection.
//!   * `webkitEntries` is an empty frozen array for the same reason (entries exist only for files a
//!     user dropped or picked).
//!   * `showPicker()` always throws `NotAllowedError`. That is not a stub: the spec's own step 2 is
//!     "if this's relevant global object does not have transient activation, throw NotAllowedError",
//!     and a headless engine driving script with no user gesture never has transient activation.
//!     Chrome 151 throws exactly this for the same reason.
//!   * `validity.tooLong` / `tooShort` are always false and `badInput` is always false. All three are
//!     defined in terms of a value the USER edited through the control ("the element's value was last
//!     set by the user" / "the control has input the UA cannot convert"). Script-set values never
//!     satisfy that condition in any engine, so false is the correct answer here, not a placeholder.
//!   * `reportValidity()` reports the same verdict as `checkValidity()` but cannot show the browser's
//!     validation bubble or focus the control — there is no widget layer to show it in.
//!   * `validationMessage` uses Chrome's en-US wording for the constraints it can evaluate and a
//!     generic "Please enter a valid value." for the rest; the messages are UI strings, not API
//!     contract, and Korvus has no localization catalogue.

use deno_core::v8;
use crate::modules::crypto::shared::crypto_num;
use crate::core::css::css_style_declaration::priv_get as wrapper_priv_get;
use crate::core::dom::element::{id_ref_in_tree, priv_set as wrapper_priv_set};

/// A control's state -- its selection, its files, its dirty value -- is the NODE's, as Blink keeps it on the element:
/// read and written on the page's own object for it, whichever world's object a script holds (Playwright's utility
/// world calls `select()` and assigns `files` from an isolated world, and the page reads them back).
fn priv_get<'s>(scope: &mut v8::PinScope<'s, '_>, o: v8::Local<v8::Object>, name: &str) -> Option<v8::Local<'s, v8::Value>> {
    let o = crate::core::html::isolated_world::page_object(scope, o);
    wrapper_priv_get(scope, o, name)
}

fn priv_set(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, name: &str, v: v8::Local<v8::Value>) {
    let o = crate::core::html::isolated_world::page_object(scope, o);
    wrapper_priv_set(scope, o, name, v)
}
use crate::core::fileapi::FileList;
use crate::binding::protos::{
    build_fn, build_fn_data, int_val, require_args, shell_proto, str_val, throw_dom, throw_type_error, unwrap_id,
};

// ── the type keyword ────────────────────────────────────────────────────────────────────────────
//
// `type` is `[CEReactions] attribute DOMString` with the "limited to only known values" clamp: an
// absent or unrecognised `type` content attribute IS `text`, which is why `input.type = "x"` reads
// back "text" rather than "x" (the attribute keeps "x"; the IDL attribute limits).
use korvus_dom::forms::{civil_from_days, days_from_civil, supports_number, supports_pattern, value_to_number, INPUT_TYPES};
#[cfg(test)]
use korvus_dom::forms::{parse_html_number, parse_week_ms, valid_email};

fn input_type(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> String {
    let a = str_val(scope, "type");
    let raw = match crate::core::dom::element::attr_call(scope, this.into(), "getAttribute", &[a]) {
        Some(v) if !v.is_null_or_undefined() => v.to_rust_string_lossy(scope),
        _ => return "text".to_string(),
    };
    let lower = raw.trim_matches(|c: char| c.is_ascii_whitespace()).to_ascii_lowercase();
    match INPUT_TYPES.iter().find(|t| **t == lower) {
        Some(t) => (*t).to_string(),
        None => "text".to_string(),
    }
}

/// The text-selection API (`selectionStart`/`End`/`Direction`, `select`, `setSelectionRange`,
/// `setRangeText`) applies only to the five text-ish types — HTML §4.10.5.3 "Common input element
/// APIs". On any other type the attributes are `null` and the methods throw `InvalidStateError`.
fn supports_selection(ty: &str) -> bool { matches!(ty, "text" | "search" | "url" | "tel" | "password") }
/// `valueAsDate` applies to the four types whose value denotes a point (or span) on the calendar.
/// `datetime-local` is deliberately NOT one of them: it has no timezone, so it denotes no instant.
fn supports_date(ty: &str) -> bool { matches!(ty, "date" | "month" | "week" | "time") }
// ── small v8 conveniences ───────────────────────────────────────────────────────────────────────

fn has_attr(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, name: &str) -> bool {
    let a = str_val(scope, name);
    crate::core::dom::element::attr_call(scope, this.into(), "hasAttribute", &[a]).map(|v| v.boolean_value(scope)).unwrap_or(false)
}
fn attr(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, name: &str) -> Option<String> {
    let a = str_val(scope, name);
    match crate::core::dom::element::attr_call(scope, this.into(), "getAttribute", &[a]) {
        Some(v) if !v.is_null_or_undefined() => Some(v.to_rust_string_lossy(scope)),
        _ => None,
    }
}
/// The `value` member of the control the shared steps run on (`<input>` or `<textarea>`), as the realm's own
/// getter / setter: `valueAsNumber`, `stepUp`, `select()`, `setRangeText()` read and write the control's value
/// in Blink without calling anything a page can wrap, and these went through the page's `value`
/// (`tests/engine_reads_live.rs`).
fn value_member(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> &'static str {
    let tag = crate::page::tag_name_of(scope, this.into()).unwrap_or_default();
    if tag.eq_ignore_ascii_case("textarea") { "HTMLTextAreaElement.value" } else { "HTMLInputElement.value" }
}
fn input_value(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> String {
    let m = value_member(scope, this);
    crate::binding::protos::pristine_get(scope, this.into(), m).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default()
}
fn set_input_value(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, s: &str) {
    let sv = str_val(scope, s);
    let m = value_member(scope, this);
    crate::binding::protos::pristine_set(scope, this.into(), m, sv);
}
fn slot_u32(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, name: &str) -> Option<u32> {
    priv_get(scope, this, name).and_then(|v| v.uint32_value(scope))
}
fn set_slot_u32(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, name: &str, n: u32) {
    let v = int_val(scope, n as i64);
    priv_set(scope, this, name, v);
}
fn slot_str(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, name: &str) -> Option<String> {
    priv_get(scope, this, name).map(|v| v.to_rust_string_lossy(scope))
}

// Per-element state that is NOT a content attribute. Private v8 slots on the interned wrapper: they
// never appear in getOwnPropertyNames/for-in, so the element's own-property surface stays exactly what
// a real HTMLInputElement's is. (Same mechanism as CSD_EID / the <model> element's slots.)
const SEL_START_PRIV: &str = "korvus_input_sel_start";
const SEL_END_PRIV: &str = "korvus_input_sel_end";
const SEL_DIR_PRIV: &str = "korvus_input_sel_dir";
const FILES_PRIV: &str = "korvus_input_files";
const VALIDITY_PRIV: &str = "korvus_input_validity";
/// The ValidityState -> input back-reference (read by every flag getter).
const VALIDITY_OWNER_PRIV: &str = "korvus_validity_owner";

// ── date / number conversions ───────────────────────────────────────────────────────────────────
//
// The `value` of a temporal input is a string in a spec-defined format, and `valueAsNumber` /
// `valueAsDate` / `stepUp` are defined as conversions to and from it. These are those conversions;
// they are pure string<->number, no timezone (every temporal input value is UTC-based by definition).

/// The reverse of [`parse_week_ms`].
fn format_week(ms: f64) -> Option<String> {
    let days = (ms / 86_400_000.0).floor() as i64;
    let (y0, _, _) = civil_from_days(days);
    // The week's year is the year owning its Thursday, so probe the neighbouring years.
    for y in [y0 + 1, y0, y0 - 1] {
        let jan4 = days_from_civil(y, 1, 4);
        let dow = (jan4 + 3).rem_euclid(7);
        let start = jan4 - dow;
        let w = (days - start) / 7 + 1;
        if days >= start && (1..=53).contains(&w) {
            let jan4n = days_from_civil(y + 1, 1, 4);
            if days < jan4n - (jan4n + 3).rem_euclid(7) {
                return Some(format!("{y:04}-W{w:02}"));
            }
        }
    }
    None
}

fn format_time(ms: f64) -> String {
    let total = ms.rem_euclid(86_400_000.0).round() as i64;
    let (h, mi, s, f) = (total / 3_600_000, total / 60_000 % 60, total / 1000 % 60, total % 1000);
    if f != 0 { format!("{h:02}:{mi:02}:{s:02}.{f:03}") }
    else if s != 0 { format!("{h:02}:{mi:02}:{s:02}") }
    else { format!("{h:02}:{mi:02}") }
}
fn format_date(days: i64) -> String {
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// "convert a number to a string" (HTML §4.10.5.4). `None` when the number has no representation in
/// this type's value syntax, which the spec turns into an `InvalidStateError`.
/// The widest day offset a date-typed value can represent: ECMAScript's time-value limit,
/// +/-8.64e15 ms, is +/-100,000,000 days from the epoch. A number outside it has no representation
/// in any date syntax, which is the spec's `None` -> InvalidStateError -- and, just as importantly,
/// keeps `days * 86_400_000` inside i64.
///
/// Without the bound, `input.valueAsNumber = 1e300` on a `datetime-local` PANICKED: the division
/// produced ~1.16e292, `as i64` saturated it to i64::MAX, and the multiply overflowed. Infinity and
/// NaN were already rejected by the setter, so it was only ever the finite-but-enormous case, which
/// is exactly what WPT's input-valueasnumber-typeerror.html sets.
const MAX_DAY_OFFSET: f64 = 100_000_000.0;

fn number_to_value(scope: &mut v8::PinScope, ty: &str, n: f64) -> Option<String> {
    if !n.is_finite() { return None; }
    if matches!(ty, "date" | "week" | "month" | "datetime-local")
        && (n / 86_400_000.0).abs() > MAX_DAY_OFFSET
    {
        return None;
    }
    match ty {
        // The shortest round-tripping decimal — V8's own Number::toString, so `0.1 + 0.2` prints the
        // same here as it does in JS rather than through a Rust formatter with different rules.
        "number" | "range" => Some(crypto_num(scope, n).to_rust_string_lossy(scope)),
        "date" => {
            if n.rem_euclid(86_400_000.0) != 0.0 { return None; }
            Some(format_date((n / 86_400_000.0) as i64))
        }
        "month" => {
            let months = n.trunc() as i64;
            let (y, m) = (1970 + months.div_euclid(12), months.rem_euclid(12) + 1);
            Some(format!("{y:04}-{m:02}"))
        }
        "week" => format_week(n),
        "time" => Some(format_time(n)),
        "datetime-local" => {
            let days = (n / 86_400_000.0).floor() as i64;
            // The remainder is taken in f64 -- `days * 86_400_000` in i64 is what used to overflow.
            Some(format!("{}T{}", format_date(days), format_time(n - days as f64 * 86_400_000.0)))
        }
        _ => None,
    }
}

// ── type, formEnctype, formMethod … are plain reflections; they live in the JS install ──────────
// ── the members that need real logic follow ─────────────────────────────────────────────────────

/// `checked` — the element's CHECKEDNESS, which is NOT the `checked` content attribute.
///
/// Until something sets it, checkedness tracks the content attribute (that is what `checked=""` in
/// markup does). The first assignment sets the "dirty checkedness flag" and the two part company
/// forever: `input.checked = true` must NOT add a `checked` attribute, or `defaultChecked` — which IS
/// the attribute — would move with it, and a form reset would have nothing to reset back to.
pub(crate) fn cb_input_checked_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = unwrap_id(scope, args.this().into()) else { rv.set_bool(false); return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let v = op_state.borrow().borrow::<crate::binding::host_dom::HostDom>().0.checkedness(id);
    rv.set_bool(v);
}
/// `checked = b` sets the checkedness and its dirty flag in the arena (`korvus_dom::forms`), where the
/// cascade's `:checked` reads it too; a radio turned on turns the rest of its group off.
pub(crate) fn cb_input_checked_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let b = args.get(0).boolean_value(scope);
    let Some(id) = unwrap_id(scope, args.this().into()) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    op_state.borrow_mut().borrow_mut::<crate::binding::host_dom::HostDom>().0.set_checkedness(id, b);
}

/// `indeterminate` — per-element state with no content attribute at all, kept in the arena so
/// `:indeterminate` sees it.
pub(crate) fn cb_input_indeterminate_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = unwrap_id(scope, args.this().into()) else { rv.set_bool(false); return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let v = op_state.borrow().borrow::<crate::binding::host_dom::HostDom>().0.indeterminate(id);
    rv.set_bool(v);
}
pub(crate) fn cb_input_indeterminate_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let b = args.get(0).boolean_value(scope);
    let Some(id) = unwrap_id(scope, args.this().into()) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    op_state.borrow_mut().borrow_mut::<crate::binding::host_dom::HostDom>().0.set_indeterminate(id, b);
}

/// `form` — the element's FORM OWNER: the form named by the `form` content attribute if there is one,
/// otherwise the nearest ancestor `form`. Readonly; null for a detached input, which is why a freshly
/// `createElement`ed one answers null.
pub(crate) fn cb_input_form_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match form_owner_of(scope, args.this()) {
        Some(f) => rv.set(f),
        None => rv.set_null(),
    }
}

/// THE form-owner production: `korvus_dom::forms::form_owner`, which is where the whole rule already
/// lived -- the `form=` content attribute's target in the same tree when there is one (an explicit
/// `form=""` means NO owner, not "look at my ancestors"), else the nearest ancestor `<form>`.
///
/// `pub(crate)` because three IDL members are the same question and were three separate answers:
/// `<input>.form` (here), `label.form` (the form owner of the label's CONTROL) and
/// `ElementInternals.form`, which asked `host.closest("form")` and so ignored `form=` entirely.
pub(crate) fn form_owner_of<'s>(scope: &mut v8::PinScope<'s, '_>, el: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::Value>> {
    let id = unwrap_id(scope, el.into())?;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let owner = { op_state.borrow().borrow::<crate::binding::host_dom::HostDom>().0.form_owner(id) };
    let f = owner?;
    Some(crate::binding::protos::wrap_cached(scope, &op_state, f).into())
}

fn is_tag(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, tag: &str) -> bool {
    crate::page::tag_name_of(scope, v).is_some_and(|t| t.eq_ignore_ascii_case(tag))
}

/// `list` — the `<datalist>` the `list` content attribute names, in this element's tree. Readonly and
/// null unless the id resolves to an actual `datalist` (a `list` pointing at a `<div>` is no list).
pub(crate) fn cb_input_list_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let ty = input_type(scope, this);
    // The types that have no suggestions source at all ignore `list` entirely.
    if matches!(ty.as_str(), "hidden" | "password" | "checkbox" | "radio" | "file" | "submit" | "image" | "reset" | "button") {
        rv.set_null();
        return;
    }
    let id = match attr(scope, this, "list") { Some(i) => i, None => { rv.set_null(); return } };
    match id_ref_in_tree(scope, this.into(), &id).filter(|el| is_tag(scope, *el, "datalist")) {
        Some(el) => rv.set(el),
        None => rv.set_null(),
    }
}

/// `labels` — every `<label>` in this element's tree whose labeled control is this input, in tree
/// order. `null` for `type=hidden`, which is not a labelable element.
pub(crate) fn cb_input_labels_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let this = args.this();
    if input_type(scope, this) == "hidden" { rv.set_null(); return; }
    match labels_of(scope, this) {
        Some(l) => rv.set(l),
        None => rv.set_null(),
    }
}

/// THE `labels` production: every `<label>` in `el`'s node tree whose labeled control is `el`, as a
/// static NodeList. One line, EIGHT interfaces — `<input>`, `<button>`, `<meter>`, `<output>`,
/// `<progress>`, `<select>`, `<textarea>` and `ElementInternals.labels`.
///
/// It asks `html_label_element.rs::label_control`, the same function `label.control` answers with,
/// instead of re-deciding what a label points at. The copy that used to live here got the two
/// halves of the rule right but carried its own frozen labelable selector, so a label wrapping a
/// FORM-ASSOCIATED CUSTOM ELEMENT was invisible from this end while `label.control` saw it — and
/// `label.control === c` while `c.labels` is empty is a contradiction no browser has.
pub(crate) fn labels_of<'s>(scope: &mut v8::PinScope<'s, '_>, el: v8::Local<'s, v8::Object>) -> Option<v8::Local<'s, v8::Value>> {
    // the realm's own tree queries, never the page's
    let root = crate::binding::protos::pristine_call(scope, el.into(), "Node.getRootNode", &[])?;
    let sel = str_val(scope, "label");
    // A NodeList is NOT a `v8::Array` -- it is a branded object with a `length` and indexed keys, so
    // it is walked through `length`/`get_index`. `try_from::<v8::Array>` on it FAILS, which is how
    // the copy this replaces answered an empty list to `input.labels` for every input in the tree
    // (WPT `the-input-element/input-labels.html` and `the-button-element/button-labels.html`, 0/1
    // each at 0162581b).
    let all = crate::binding::protos::pristine_query_all(scope, root, sel);
    let mut out: Vec<v8::Local<v8::Value>> = Vec::new();
    if let Some(all) = all {
        let n = crate::binding::protos::pristine_get(scope, all.into(), "NodeList.length").and_then(|v| v.uint32_value(scope)).unwrap_or(0);
        for i in 0..n {
            let Some(label) = all.get_index(scope, i) else { continue };
            let Ok(label_obj) = v8::Local::<v8::Object>::try_from(label) else { continue };
            if crate::core::html::html_label_element::label_control(scope, label_obj) == Some(el.into()) {
                out.push(label);
            }
        }
    }
    let arr = v8::Array::new(scope, out.len() as i32);
    for (i, l) in out.iter().enumerate() { arr.set_index(scope, i as u32, *l); }
    Some(crate::core::dom::collections::make_static(scope, "NodeList", arr))
}

/// `labels` for the OTHER labelable elements -- `<meter>`, `<output>`, `<progress>`, `<select>`,
/// `<textarea>`, `<button>`. The `for`-attribute-versus-containment resolution is identical for every
/// labelable element, so this is the same body rather than a second copy of it; the only input-only
/// rule is the `type=hidden` carve-out, and `input_type` reports "text" for any element that is not an
/// `<input>` at all, so that branch is already unreachable from here. Named without `input_` so the JS
/// side does not read as if a `<meter>` were one.

/// `files` — the selected file list. `null` on every type but `file`; on `type=file` a real (empty)
/// `FileList`, created once and kept, because the spec speaks of THE selected files (one object).
pub(crate) fn cb_input_files_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    if input_type(scope, this) != "file" { rv.set_null(); return; }
    if let Some(existing) = priv_get(scope, this, FILES_PRIV) {
        if !existing.is_null() { rv.set(existing); return; }
    }
    // Minted through the FileList table's mint site, so the instance carries the
    // TABLE's prototype (`input.files instanceof FileList`, `constructor`, `Object.getPrototypeOf`)
    // rather than the cppgc class template's.
    let fl = crate::core::fileapi::file_list::mint_file_list(scope, Vec::new());
    let v: v8::Local<v8::Value> = fl.into();
    priv_set(scope, this, FILES_PRIV, v);
    rv.set(v);
}
pub(crate) fn cb_input_files_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let v = args.get(0);
    if v.is_null_or_undefined() {
        let n: v8::Local<v8::Value> = v8::null(scope).into();
        priv_set(scope, this, FILES_PRIV, n);
        if let Some(id) = unwrap_id(scope, this.into()) {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            op_state.borrow_mut().borrow_mut::<crate::binding::host_dom::HostDom>().0.set_file_count(id, 0);
        }
        return;
    }
    let Some(list) = deno_core::cppgc::try_unwrap_cppgc_object::<FileList>(scope, v) else {
        throw_type_error(scope, "Failed to set the 'files' property on 'HTMLInputElement': The provided value is not of type 'FileList'.");
        return;
    };
    let count = unsafe { list.as_ref() }.files.borrow().len() as u32;
    priv_set(scope, this, FILES_PRIV, v);
    if let Some(id) = unwrap_id(scope, this.into()) {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        op_state.borrow_mut().borrow_mut::<crate::binding::host_dom::HostDom>().0.set_file_count(id, count);
    }
}

/// The files `input` has selected -- each one's name, type and bytes -- for its form's submission. Empty for a control
/// with none (or no file control at all).
pub(crate) fn selected_files(scope: &mut v8::PinScope, input: v8::Local<v8::Object>) -> Vec<crate::core::html::form_encoding::FilePart> {
    let Some(v) = priv_get(scope, input, FILES_PRIV).filter(|v| !v.is_null_or_undefined()) else { return Vec::new() };
    let Some(list) = deno_core::cppgc::try_unwrap_cppgc_object::<FileList>(scope, v) else { return Vec::new() };
    let files: Vec<v8::Global<v8::Value>> = unsafe { list.as_ref() }.files.borrow().clone();
    files
        .iter()
        .filter_map(|g| {
            let f = v8::Local::new(scope, g);
            Some(crate::core::html::form_encoding::FilePart {
                name: crate::core::fileapi::file::file_name(scope, f).unwrap_or_default(),
                ty: crate::core::fileapi::blob::blob_type(scope, f).unwrap_or_default(),
                bytes: crate::core::fileapi::blob::blob_bytes(scope, f)?,
            })
        })
        .collect()
}

/// `webkitEntries` — the `FileSystemEntry`s behind the selected files. Always an empty frozen array:
/// entries exist only for files a user dropped or picked, and no file can have been picked here.
pub(crate) fn cb_input_webkit_entries(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let _ = args;
    let arr = v8::Array::new(scope, 0);
    // FrozenArray<T>: the platform hands back a frozen array, and freezing it here is what makes
    // `Object.isFrozen(input.webkitEntries)` true the way it is in Chrome.
    arr.set_integrity_level(scope, v8::IntegrityLevel::Frozen);
    rv.set(arr.into());
}

/// `width` / `height` — the rendered dimensions. They are reflected `unsigned long` attributes, but
/// ONLY on `type=image`: for every other type the element has no intrinsic image, so the getter is 0
/// no matter what `width="…"` says. The setter always writes the attribute (that is what makes the
/// value stick when the type later becomes `image`), which is why `input.width = 1` reads back 0 on a
/// `type=text` input rather than throwing or silently dropping the write.
///
/// On `type=image` with no `width` attribute the answer is the loaded image's intrinsic width, which
/// is 0 here: Korvus has no image decoder, so no `<input type=image>` ever has intrinsic dimensions.
/// The function's data is the attribute name ("width" / "height").
pub(crate) fn cb_input_dim_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    if input_type(scope, this) != "image" { rv.set_uint32(0); return; }
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let n = attr(scope, this, &name).and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(0);
    rv.set_uint32(n);
}
pub(crate) fn cb_input_dim_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let n = args.get(0).number_value(scope).unwrap_or(0.0);
    let n = if n.is_finite() && n > 0.0 { n as u32 } else { 0 };
    let v = str_val(scope, &n.to_string());
    let name = crate::binding::protos::body_data(scope, &args);
    crate::core::dom::element::attr_call(scope, args.this().into(), "setAttribute", &[name, v]);
}

// ── text selection ──────────────────────────────────────────────────────────────────────────────
//
// The selection is a pair of offsets into the element's value plus a direction. Korvus has no caret,
// but the offsets are pure API state: they are set through the API, clamped to the value's length,
// and read back. That clamp is the whole reason `input.selectionStart = 1` on an empty input reads
// back 0 — not a missing implementation, but the spec's "if it is greater than the length of the
// element's API value, then let it be the length".

fn sel_len(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> u32 {
    // The API value's length in UTF-16 code units, which is what offsets index.
    let s = input_value(scope, this);
    s.encode_utf16().count() as u32
}
/// A text control's selection, `(start, end)` in UTF-16 code units of its value (the editing commands' selection
/// when the control is focused).
pub(crate) fn text_control_selection(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> (u32, u32) {
    (sel_get(scope, this, SEL_START_PRIV), sel_get(scope, this, SEL_END_PRIV))
}

/// Select a text control's whole value (`execCommand("selectAll")` in a focused control).
pub(crate) fn text_control_select_all(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) {
    let len = sel_len(scope, this);
    set_selection(scope, this, 0, len, "forward");
}

fn sel_get(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, slot: &str) -> u32 {
    slot_u32(scope, this, slot).unwrap_or(0).min(sel_len(scope, this))
}
/// The default direction is "forward", not "none": on platforms whose editor treats selections as
/// directional (everything but macOS) Blink starts a text control there, and an unrecognised
/// `selectionDirection` assignment lands there too — which is why `input.selectionDirection = "x"`
/// reads back "forward" rather than "none".
fn sel_dir(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> String {
    slot_str(scope, this, SEL_DIR_PRIV).unwrap_or_else(|| "forward".to_string())
}
fn normalize_dir(s: &str) -> &'static str {
    match s { "backward" => "backward", "forward" => "forward", _ => "forward" }
}
fn set_selection(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, start: u32, end: u32, dir: &str) {
    let len = sel_len(scope, this);
    let end = end.min(len);
    let start = start.min(end);
    set_slot_u32(scope, this, SEL_START_PRIV, start);
    set_slot_u32(scope, this, SEL_END_PRIV, end);
    let d = str_val(scope, normalize_dir(dir));
    priv_set(scope, this, SEL_DIR_PRIV, d);
}

/// The three selection attributes share one getter/setter pair; the data says which
/// ("start" / "end" / "direction").
pub(crate) fn cb_input_selection_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    // `unsigned long?` / `DOMString?`: null on the types with no text to select at all.
    if !supports_selection(&input_type(scope, this)) { rv.set_null(); return; }
    match crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope).as_str() {
        "start" => rv.set_uint32(sel_get(scope, this, SEL_START_PRIV)),
        "end" => rv.set_uint32(sel_get(scope, this, SEL_END_PRIV)),
        _ => { let d = sel_dir(scope, this); let v = str_val(scope, &d); rv.set(v); }
    }
}
pub(crate) fn cb_input_selection_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    if !supports_selection(&input_type(scope, this)) {
        throw_dom(scope, "The input element's type ('checkbox') does not support selection.", "InvalidStateError");
        return;
    }
    let which = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let v = args.get(0);
    let (start, end, dir) = (sel_get(scope, this, SEL_START_PRIV), sel_get(scope, this, SEL_END_PRIV), sel_dir(scope, this));
    match which.as_str() {
        "start" => {
            // `unsigned long?`: null means "collapse to the end", per the spec's own wording.
            let n = if v.is_null_or_undefined() { end } else { to_ulong(scope, v) };
            set_selection(scope, this, n, end.max(n), &dir);
        }
        "end" => {
            let n = if v.is_null_or_undefined() { 0 } else { to_ulong(scope, v) };
            set_selection(scope, this, start, n, &dir);
        }
        _ => {
            let d = if v.is_null_or_undefined() { "none".to_string() } else { v.to_rust_string_lossy(scope).to_ascii_lowercase() };
            set_selection(scope, this, start, end, &d);
        }
    }
}

/// WebIDL `unsigned long`: ToNumber, then modulo 2^32 (so a negative or fractional assignment wraps
/// exactly the way the binding layer would make it).
fn to_ulong(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> u32 {
    let n = v.number_value(scope).unwrap_or(0.0);
    if !n.is_finite() { return 0; }
    (n.trunc().rem_euclid(4_294_967_296.0)) as u32
}

/// `createValueRange(start, end)` (Chrome 152, `TextControlElement::createValueRange`): an `OpaqueRange` over the
/// value. An `<input>` must be a text field type (NotSupportedError); an offset past the value's length (UTF-16)
/// is an IndexSizeError, and a backwards range collapses to its start, as a `Range` does.
pub(crate) fn cb_create_value_range(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let Some(id) = crate::binding::protos::unwrap_id(scope, this.into()) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let textarea = op_state.borrow().borrow::<crate::binding::host_dom::HostDom>().0.tag_name(id).is_some_and(|t| t.eq_ignore_ascii_case("textarea"));
    let iface = if textarea { "HTMLTextAreaElement" } else { "HTMLInputElement" };
    if !require_args(scope, &args, 2, iface, "createValueRange") { return; }
    let start = to_ulong(scope, args.get(0));
    let mut end = to_ulong(scope, args.get(1));
    if !textarea && !supports_selection(&input_type(scope, this)) {
        throw_dom(scope, "Failed to execute 'createValueRange' on 'HTMLInputElement': <input> element must be of a text field type: text, search, url, tel, or password.", "NotSupportedError");
        return;
    }
    let len = input_value(scope, this).encode_utf16().count() as u32;
    if start > len || end > len {
        throw_dom(scope, &format!("Failed to execute 'createValueRange' on '{iface}': Start or end offset exceeds value length."), "IndexSizeError");
        return;
    }
    if start > end {
        end = start;
    }
    let rid = op_state.borrow_mut().borrow_mut::<crate::binding::host_dom::HostDom>().0.create_opaque_range(id, start, end);
    if let Some(r) = crate::core::dom::abstract_range::new_opaque_range(scope, rid) {
        rv.set(r.into());
    }
}

/// `select()` — select the whole value. A no-op (not an error) on types with nothing to select.
pub(crate) fn cb_input_select(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    if !supports_selection(&input_type(scope, this)) { return; }
    let len = sel_len(scope, this);
    set_selection(scope, this, 0, len, "none");
}

/// `setSelectionRange(start, end, direction)`.
pub(crate) fn cb_input_set_selection_range(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !require_args(scope, &args, 2, "HTMLInputElement", "setSelectionRange") { return; }
    let this = args.this();
    if !supports_selection(&input_type(scope, this)) {
        throw_dom(scope, "The input element's type does not support selection.", "InvalidStateError");
        return;
    }
    let start = to_ulong(scope, args.get(0));
    let end = to_ulong(scope, args.get(1));
    let dir = if args.length() > 2 && !args.get(2).is_undefined() { args.get(2).to_rust_string_lossy(scope).to_ascii_lowercase() } else { "none".to_string() };
    set_selection(scope, this, start, end, &dir);
}

/// `setRangeText(replacement[, start, end, selectionMode])` — a real splice of the element's value.
pub(crate) fn cb_input_set_range_text(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !require_args(scope, &args, 1, "HTMLInputElement", "setRangeText") { return; }
    let this = args.this();
    if !supports_selection(&input_type(scope, this)) {
        throw_dom(scope, "The input element's type does not support selection.", "InvalidStateError");
        return;
    }
    let replacement: Vec<u16> = args.get(0).to_rust_string_lossy(scope).encode_utf16().collect();
    let value: Vec<u16> = input_value(scope, this).encode_utf16().collect();
    let len = value.len() as u32;
    // The 1-argument form splices over the CURRENT selection; the 3+-argument form over the range given.
    let (mut start, mut end) = if args.length() >= 3 {
        (to_ulong(scope, args.get(1)), to_ulong(scope, args.get(2)))
    } else {
        (sel_get(scope, this, SEL_START_PRIV), sel_get(scope, this, SEL_END_PRIV))
    };
    if start > end {
        throw_dom(scope, "The provided start value (which is larger than the provided end value) is invalid.", "IndexSizeError");
        return;
    }
    start = start.min(len);
    end = end.min(len);
    let mut next: Vec<u16> = Vec::with_capacity(value.len() + replacement.len());
    next.extend_from_slice(&value[..start as usize]);
    next.extend_from_slice(&replacement);
    next.extend_from_slice(&value[end as usize..]);
    let new_value = String::from_utf16_lossy(&next);
    // The selection "preserve" mode below is defined over the selection BEFORE the splice, and
    // assigning the value moves the cursor to the end, so it is read first.
    let (old_start, old_end, old_dir) =
        (sel_get(scope, this, SEL_START_PRIV) as i64, sel_get(scope, this, SEL_END_PRIV) as i64, sel_dir(scope, this));
    // the control's `OpaqueRange`s move by this splice, not by a whole-value diff (`setRangeText`'s
    // `ScopedSkipValueAutoDiff` + `CommitProgrammaticOpaqueRangeEdit(original, start, end)`, Chrome 152)
    if let Some(id) = crate::binding::protos::unwrap_id(scope, this.into()) {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        op_state.borrow_mut().borrow_mut::<crate::binding::host_dom::HostDom>().0.set_value_edit_bounds(id, start, end);
    }
    set_input_value(scope, this, &new_value);

    let new_end = start + replacement.len() as u32;
    let mode = if args.length() >= 4 && !args.get(3).is_undefined() { args.get(3).to_rust_string_lossy(scope) } else { "preserve".to_string() };
    match mode.as_str() {
        "select" => set_selection(scope, this, start, new_end, "none"),
        "start" => set_selection(scope, this, start, start, "none"),
        "end" => set_selection(scope, this, new_end, new_end, "none"),
        // "preserve": keep the old selection, shifted by however much the value grew or shrank.
        _ => {
            let delta = new_end as i64 - end as i64;
            let shift = |o: i64| -> u32 { if o > end as i64 { (o + delta).max(0) as u32 } else { o.max(0) as u32 } };
            set_selection(scope, this, shift(old_start), shift(old_end), &old_dir);
        }
    }
}

// ── valueAsNumber / valueAsDate / stepUp / stepDown ─────────────────────────────────────────────

pub(crate) fn cb_input_value_as_number_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let ty = input_type(scope, this);
    // "If the valueAsNumber attribute does not apply, then return NaN" — no throw on GETTING.
    let n = if supports_number(&ty) { value_to_number(&ty, &input_value(scope, this)) } else { f64::NAN };
    rv.set(crypto_num(scope, n));
}

pub(crate) fn cb_input_value_as_number_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let ty = input_type(scope, this);
    if !supports_number(&ty) {
        throw_dom(scope, &format!("Failed to set the 'valueAsNumber' property on 'HTMLInputElement': This input element does not support Number values. (type='{ty}')"), "InvalidStateError");
        return;
    }
    let n = args.get(0).number_value(scope).unwrap_or(f64::NAN);
    if n.is_infinite() {
        throw_type_error(scope, "Failed to set the 'valueAsNumber' property on 'HTMLInputElement': The value provided is infinite.");
        return;
    }
    // "If the new value is NaN, then set the value to the empty string" — the documented way to clear.
    if n.is_nan() { set_input_value(scope, this, ""); return; }
    match number_to_value(scope, &ty, n) {
        Some(s) => set_input_value(scope, this, &s),
        None => throw_dom(scope, "Failed to set the 'valueAsNumber' property on 'HTMLInputElement': The value provided cannot be converted to a valid value for this type.", "InvalidStateError"),
    }
}

/// `valueAsDate` — the value as a `Date`, for the four types that denote a calendar point.
/// A `month` value's Date is midnight UTC on the first day of that month, not a month count: the
/// number space and the date space differ for that one type, so the two accessors do not share a path.
fn value_to_date_ms(ty: &str, s: &str) -> f64 {
    match ty {
        "month" => {
            let n = value_to_number("month", s);
            if n.is_nan() { return f64::NAN; }
            let months = n as i64;
            (days_from_civil(1970 + months.div_euclid(12), months.rem_euclid(12) + 1, 1) * 86_400_000) as f64
        }
        _ => value_to_number(ty, s),
    }
}
pub(crate) fn cb_input_value_as_date_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let ty = input_type(scope, this);
    if !supports_date(&ty) { rv.set_null(); return; }
    let ms = value_to_date_ms(&ty, &input_value(scope, this));
    if ms.is_nan() { rv.set_null(); return; }
    let d = v8::Date::new(scope, ms).map(|d| v8::Local::<v8::Value>::from(d));
    match d { Some(v) => rv.set(v), None => rv.set_null() }
}
pub(crate) fn cb_input_value_as_date_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let v = args.get(0);
    // Chrome declares this `attribute Date? valueAsDate`, so the BINDING rejects a non-Date before the
    // element is ever consulted: `input.valueAsDate = {}` is a TypeError even on a type that would
    // have raised InvalidStateError. Order matters, and this is the order.
    if !v.is_null_or_undefined() && !v.is_date() {
        throw_type_error(scope, "Failed to set the 'valueAsDate' property on 'HTMLInputElement': The provided value is not of type 'Date'.");
        return;
    }
    let ty = input_type(scope, this);
    if !supports_date(&ty) {
        throw_dom(scope, "Failed to set the 'valueAsDate' property on 'HTMLInputElement': This input element does not support Date values.", "InvalidStateError");
        return;
    }
    if v.is_null_or_undefined() { set_input_value(scope, this, ""); return; }
    let ms = v.number_value(scope).unwrap_or(f64::NAN);
    if ms.is_nan() { set_input_value(scope, this, ""); return; }
    let text = match ty.as_str() {
        // Back from an instant to a month string: the month containing that instant.
        "month" => { let (y, m, _) = civil_from_days((ms / 86_400_000.0).floor() as i64); Some(format!("{y:04}-{m:02}")) }
        _ => number_to_value(scope, &ty, ms),
    };
    match text { Some(s) => set_input_value(scope, this, &s), None => set_input_value(scope, this, "") }
}

/// `stepUp(n)` / `stepDown(n)` — walk the value by n steps. The data is the sign.
pub(crate) fn cb_input_step_by(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let sign = crate::binding::protos::body_data(scope, &args).number_value(scope).unwrap_or(1.0);
    let ty = input_type(scope, this);
    // Step 1 of the spec's algorithm: "If the stepUp() and stepDown() methods do not apply, then throw
    // an InvalidStateError" — which is every non-numeric, non-temporal type.
    if !supports_number(&ty) {
        throw_dom(scope, "Failed to execute 'stepUp' on 'HTMLInputElement': This form element is not steppable.", "InvalidStateError");
        return;
    }
    let Some(id) = unwrap_id(scope, this.into()) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (allowed, base) = {
        let st = op_state.borrow();
        let dom = &st.borrow::<crate::binding::host_dom::HostDom>().0;
        (dom.allowed_step(id, &ty), dom.step_base(id, &ty))
    };
    let step = match allowed {
        Some(s) => s,
        // step="any": "if the allowed value step is any, then throw an InvalidStateError".
        None => { throw_dom(scope, "Failed to execute 'stepUp' on 'HTMLInputElement': This form element does not have an allowed value step.", "InvalidStateError"); return; }
    };
    let n = if args.length() > 0 && !args.get(0).is_undefined() { args.get(0).number_value(scope).unwrap_or(1.0).trunc() } else { 1.0 };
    let current = value_to_number(&ty, &input_value(scope, this));
    // "If value subtracted from the step base is not an integral multiple of the allowed value step,
    // then round it to the nearest step" before applying n.
    let start = if current.is_nan() { 0.0 } else { current };
    let aligned = base + ((start - base) / step).round() * step;
    let mut next = aligned + sign * n * step;
    // Clamp into [min, max] the way the algorithm's steps 9-11 do.
    if let Some(mx) = attr(scope, this, "max").map(|s| value_to_number(&ty, s.trim())).filter(|v| !v.is_nan()) {
        if next > mx { next = base + ((mx - base) / step).floor() * step; }
    }
    if let Some(mn) = attr(scope, this, "min").map(|s| value_to_number(&ty, s.trim())).filter(|v| !v.is_nan()) {
        if next < mn { next = base + ((mn - base) / step).ceil() * step; }
    }
    match number_to_value(scope, &ty, next) {
        Some(s) => set_input_value(scope, this, &s),
        None => throw_dom(scope, "Failed to execute 'stepUp' on 'HTMLInputElement': The resulting value is not a valid value for this type.", "InvalidStateError"),
    }
}

/// `showPicker()` — HTML §4.10.5.4. Step 1 is a mutability check, step 2 is the transient-activation
/// check. Korvus has no user gesture to grant transient activation, so a mutable control reaches step 2
/// and stops there. Chrome 151 throws the identical NotAllowedError for the identical reason.
pub(crate) fn cb_input_show_picker(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    if has_attr(scope, this, "disabled") || has_attr(scope, this, "readonly") {
        throw_dom(scope, "HTMLInputElement::showPicker() failed: the element is not mutable.", "InvalidStateError");
        return;
    }
    throw_dom(scope, "HTMLInputElement::showPicker() requires a user gesture.", "NotAllowedError");
}

// ── constraint validation ───────────────────────────────────────────────────────────────────────

/// A candidate for constraint validation: HTML §4.10.20.2's "barred from constraint validation" list,
/// restricted to what a tree-only engine can see.
///
/// Not modelled: an ancestor `<fieldset disabled>` outside the element's own first `<legend>` also
/// bars it. Korvus's element glue has no "disabled ancestor" walk, and doing it here would need a
/// legend-aware ancestor scan that belongs on HTMLFieldSetElement, not on input.
/// Is `this` a candidate for constraint validation (`willValidate`)? The arena's answer
/// (`korvus_dom::forms`), which `:valid`/`:invalid` read too -- it includes the `<fieldset disabled>`
/// ancestor this file used to leave out.
fn is_validation_candidate(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> bool {
    let Some(id) = unwrap_id(scope, this.into()) else { return false };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let v = op_state.borrow().borrow::<crate::binding::host_dom::HostDom>().0.will_validate(id);
    v
}

/// `pattern`: the value must match the WHOLE pattern, as an ECMAScript regular expression with the `v`
/// flag. `None` when the pattern does not compile ("the constraint is not applied").
///
/// V8's own RegExp, made and run as Blink's `ScriptRegexp` does (`v8::RegExp::New` / `Exec`): a pattern check
/// looked up `RegExp.prototype.test` -- and `test` looks up `exec` -- on what a page may have replaced.
fn pattern_matches(scope: &mut v8::PinScope, pattern: &str, value: &str) -> Option<bool> {
    let src = v8::String::new(scope, &format!("^(?:{pattern})$"))?;
    let re = {
        v8::tc_scope!(let tc, scope);
        let re = v8::RegExp::new(tc, src, v8::RegExpCreationFlags::UNICODE_SETS).map(|r| v8::Global::new(tc, r));
        tc.reset();
        re
    }?;
    let re = v8::Local::new(scope, &re);
    let subject = v8::String::new(scope, value)?;
    let m = {
        v8::tc_scope!(let tc, scope);
        let m = re.exec(tc, subject).map(|o| !o.is_null());
        tc.reset();
        m
    };
    Some(m.unwrap_or(false))
}

/// Tell the arena whether `id`'s value matches its `pattern`, if it has one that applies and the
/// arena has not been told about this (pattern, value) pair yet. The ONLY place a `pattern` is
/// evaluated: `ValidityState` reads call it first, and so does every write that can change the answer
/// -- an attribute write (`pattern`, `value`, `type`) and an insertion or parse that brings an input in
/// -- so `:invalid` in the cascade, which cannot run a regular expression, finds the answer waiting.
/// An uncompilable pattern is recorded as a match: no constraint.
pub(crate) fn note_pattern(scope: &mut v8::PinScope, id: u32) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let pending = {
        let st = op_state.borrow();
        let Some(host) = st.try_borrow::<crate::binding::host_dom::HostDom>() else { return };
        let dom = &host.0;
        if !dom.is_html_ns(id) || dom.tag_name(id) != Some("input") {
            return;
        }
        let Some(pattern) = dom.get_attribute(id, "pattern").map(str::to_string) else { return };
        let value = dom.control_value(id);
        if value.is_empty() || !supports_pattern(dom.input_type(id)) || dom.has_pattern_result(&pattern, &value) {
            return;
        }
        (pattern, value)
    };
    let ok = pattern_matches(scope, &pending.0, &pending.1).unwrap_or(true);
    op_state.borrow_mut().borrow_mut::<crate::binding::host_dom::HostDom>().0.set_pattern_result(&pending.0, &pending.1, ok);
}

/// [`note_pattern`] for every `<input pattern>` in `root`'s subtree, `root` included.
pub(crate) fn note_patterns_under(scope: &mut v8::PinScope, root: u32) {
    let ids: Vec<u32> = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let Some(host) = st.try_borrow::<crate::binding::host_dom::HostDom>() else { return };
        let dom = &host.0;
        std::iter::once(root)
            .chain(dom.descendant_elements(root))
            .filter(|&n| dom.tag_name(n) == Some("input") && dom.get_attribute(n, "pattern").is_some())
            .collect()
    };
    for id in ids {
        note_pattern(scope, id);
    }
}

/// The element's constraint-validation flags, from the arena, with its `pattern` answered first.
fn compute_flags(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> korvus_dom::forms::Validity {
    let Some(id) = unwrap_id(scope, this.into()) else { return korvus_dom::forms::Validity::default() };
    note_pattern(scope, id);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let v = op_state.borrow().borrow::<crate::binding::host_dom::HostDom>().0.validity(id);
    v
}

/// The owner of a ValidityState object (its back-reference slot). Every flag getter starts here.
fn validity_owner<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    priv_get(scope, this, VALIDITY_OWNER_PRIV).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
}

/// One getter for all eleven `ValidityState` flags; the data names which. It reads the OWNER element
/// live, so `v = input.validity; input.value = ""; v.valueMissing` sees the new value — a
/// ValidityState is a view, not a snapshot.
fn cb_validity_flag(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let which = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let Some(owner) = validity_owner(scope, args.this()) else { rv.set_bool(false); return };
    let f = compute_flags(scope, owner);
    let v = match which.as_str() {
        "valueMissing" => f.value_missing,
        "typeMismatch" => f.type_mismatch,
        "patternMismatch" => f.pattern_mismatch,
        "rangeUnderflow" => f.range_underflow,
        "rangeOverflow" => f.range_overflow,
        "stepMismatch" => f.step_mismatch,
        "customError" => f.custom_error,
        // tooLong / tooShort / badInput are defined over a value the USER typed into the control, so
        // no `<input>` here ever sets them -- but `ElementInternals.setValidity()` sets them
        // outright, and answering a hardcoded `false` to a flag a page had just set was the ninth
        // sighting of "`None` => answer nothing" in this tree. They are arena fields now.
        "tooLong" => f.too_long,
        "tooShort" => f.too_short,
        "badInput" => f.bad_input,
        "valid" => f.valid(),
        _ => false,
    };
    rv.set_bool(v);
}

/// The eleven flags, in `ValidityState`'s WebIDL order.
const VALIDITY_FLAGS: [&str; 11] = [
    "valueMissing", "typeMismatch", "patternMismatch", "tooLong", "tooShort", "rangeUnderflow",
    "rangeOverflow", "stepMismatch", "badInput", "customError", "valid",
];
/// Marks `ValidityState.prototype` as already carrying the real flag getters.
const VALIDITY_WIRED_PRIV: &str = "korvus_validity_wired";

/// Replace `ValidityState.prototype`'s generated stubs (which answer `undefined` — worse than absent,
/// because `if (!el.validity.valid)` then silently passes) with the real native getters, once.
///
/// This happens HERE, from the `validity` getter, rather than in the JS bootstrap, because
/// `ValidityState` is one of the generated shells and those are created after every hand-written phase
/// has run (`generated_shells.js` imports `dom.js`, not the other way round), so no phase module can
/// see the prototype to patch it. The only way to obtain a ValidityState is through a control's
/// `validity`, so patching on that first access is not a race: nothing can hold an instance yet. The
/// descriptor shape (native get-only, enumerable, configurable) is identical to the stub's, so the
/// surface is the same before and after — only the answers change.
fn ensure_validity_flags<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    let proto = shell_proto(scope, "ValidityState")?;
    if priv_get(scope, proto, VALIDITY_WIRED_PRIV).is_some() { return Some(proto); }
    let marker: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    priv_set(scope, proto, VALIDITY_WIRED_PRIV, marker);
    for flag in VALIDITY_FLAGS {
        let data = str_val(scope, flag);
        let getter = build_fn_data(scope, cb_validity_flag, &format!("get {flag}"), 0, data);
        let no_setter: v8::Local<v8::Value> = v8::undefined(scope).into();
        let mut desc = v8::PropertyDescriptor::new_from_get_set(getter.into(), no_setter);
        desc.set_enumerable(true);
        desc.set_configurable(true);
        if let Some(name) = v8::String::new(scope, flag) {
            proto.define_property(scope, name.into(), &desc);
        }
    }
    Some(proto)
}

/// `validity` — the element's ValidityState. One object per element, created lazily and kept: the
/// spec speaks of THE ValidityState object, and `input.validity === input.validity` must hold.
pub(crate) fn cb_input_validity(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match validity_object_of(scope, args.this()) {
        Some(v) => rv.set(v),
        None => rv.set_null(),
    }
}

/// THE `ValidityState` production: `el`'s one ValidityState, minted on first ask and kept, with `el`
/// in its OWNER slot so every flag getter reads the element live.
///
/// `pub(crate)` because `ElementInternals.validity` is the same object for the same reason:
/// `element_internals.rs` used to mint a bare `branded_instance("ValidityState")` with NO owner slot,
/// and `cb_validity_flag` answers `false` to every flag of an ownerless one -- including `valid`, so
/// a brand-new custom control reported itself INVALID and `setValidity` could not move it.
pub(crate) fn validity_object_of<'s>(scope: &mut v8::PinScope<'s, '_>, el: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::Value>> {
    let proto = ensure_validity_flags(scope)?;
    if let Some(cached) = priv_get(scope, el, VALIDITY_PRIV) { return Some(cached); }
    let obj = crate::binding::protos::mint_on(scope, proto.into());
    let owner: v8::Local<v8::Value> = el.into();
    priv_set(scope, obj, VALIDITY_OWNER_PRIV, owner);
    let v: v8::Local<v8::Value> = obj.into();
    priv_set(scope, el, VALIDITY_PRIV, v);
    Some(v)
}

pub(crate) fn cb_input_will_validate(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let ok = is_validation_candidate(scope, args.this());
    rv.set_bool(ok);
}

/// The message a UA would show. Chrome's en-US strings for the constraints Korvus can evaluate; the
/// rest fall back to Chrome's own generic "Please enter a valid value." (these are UI strings, and
/// Korvus has no localization catalogue to draw a full set from).
pub(crate) fn cb_input_validation_message(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    // "If the element is not a candidate for constraint validation, or it is but satisfies its
    // constraints, return the empty string."
    if !is_validation_candidate(scope, this) { let e = str_val(scope, ""); rv.set(e); return; }
    let f = compute_flags(scope, this);
    if f.valid() { let e = str_val(scope, ""); rv.set(e); return; }
    let ty = input_type(scope, this);
    let msg = if f.custom_error {
        {
            let id = unwrap_id(scope, this.into()).unwrap_or(u32::MAX);
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let m = op_state.borrow().borrow::<crate::binding::host_dom::HostDom>().0.custom_validity(id).to_string();
            m
        }
    } else if f.value_missing {
        match ty.as_str() {
            "checkbox" => "Please check this box if you want to proceed.".to_string(),
            "radio" => "Please select one of these options.".to_string(),
            "file" => "Please select a file.".to_string(),
            _ => "Please fill out this field.".to_string(),
        }
    } else if f.type_mismatch && ty == "url" {
        "Please enter a URL.".to_string()
    } else if f.type_mismatch {
        "Please enter an email address.".to_string()
    } else if f.pattern_mismatch {
        "Please match the requested format.".to_string()
    } else if f.range_underflow {
        format!("Value must be greater than or equal to {}.", attr(scope, this, "min").unwrap_or_default())
    } else if f.range_overflow {
        format!("Value must be less than or equal to {}.", attr(scope, this, "max").unwrap_or_default())
    } else {
        "Please enter a valid value.".to_string()
    };
    let v = str_val(scope, &msg);
    rv.set(v);
}

/// `checkValidity()` / `reportValidity()`. Both run the same "check validity steps": a non-candidate
/// is trivially valid; a failing candidate fires a cancelable-but-unpreventable `invalid` event and
/// answers false. `reportValidity()` additionally REPORTS the problem to the user, which needs a
/// validation bubble and focus — neither exists here, so it reports the same verdict silently.
/// The data is `true` for the reporting variant.
pub(crate) fn cb_input_check_validity(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let ok = check_validity_of(scope, args.this());
    rv.set_bool(ok);
}

/// HTML's "check validity steps", and THE production for them: true when `el` is barred from
/// constraint validation or satisfies its constraints; otherwise fire `invalid` at `el` and
/// return false.
///
/// `pub(crate)` because `ElementInternals.checkValidity()` and `.reportValidity()` are the SAME
/// steps on the internals' host. They were a hardcoded `rv.set_bool(true)` -- so a custom control
/// that had just called `setValidity({customError: true})` still reported itself valid and never
/// fired `invalid`. `reportValidity` differs only in also showing the UA's validation bubble, and
/// there is no widget layer here.
pub(crate) fn check_validity_of(scope: &mut v8::PinScope, el: v8::Local<v8::Object>) -> bool {
    if !is_validation_candidate(scope, el) { return true; }
    let f = compute_flags(scope, el);
    if f.valid() { return true; }
    // "Fire an event named invalid at the element (with its cancelable attribute initialized to true)."
    if let Some(ctor) = crate::binding::protos::iface_object(scope, "Event").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) {
        let ty = str_val(scope, "invalid");
        let init = crate::binding::protos::init_dict(scope);
        let k = str_val(scope, "cancelable");
        let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
        if let Ok(key) = v8::Local::<v8::String>::try_from(k) { init.set(scope, key.into(), t); }
        if let Some(ev) = ctor.new_instance(scope, &[ty, init.into()]) {
            let evv: v8::Local<v8::Value> = ev.into();
            crate::binding::protos::engine_call(scope, el.into(), "dispatchEvent", &[evv]);
        }
    }
    false
}

pub(crate) fn cb_input_set_custom_validity(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    // The arity error names the interface the call landed on, not the file this code lives in --
    // `<button>` shares this callback.
    let iface = if is_tag(scope, args.this().into(), "button") { "HTMLButtonElement" } else { "HTMLInputElement" };
    if !require_args(scope, &args, 1, iface, "setCustomValidity") { return; }
    let s = args.get(0).to_rust_string_lossy(scope);
    let Some(id) = unwrap_id(scope, args.this().into()) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    op_state.borrow_mut().borrow_mut::<crate::binding::host_dom::HostDom>().0.set_custom_validity(id, &s);
}

// ── The same six, exposed under names that do not say `input` ───────────────────────────────────
//
// Constraint validation is a mixin: `<input>`, `<button>`, `<select>`, `<textarea>`, `<fieldset>`
// and `<output>` all carry willValidate/validity/validationMessage/checkValidity/reportValidity/
// setCustomValidity, and `form` besides. The callbacks above are already written against the
// element rather than against "an input" -- `is_validation_candidate` and `compute_flags` ask the
// arena, which branches on the tag -- so a second interface needs no second copy, only a second name. Sharing the
// callback is what keeps `<button>` from drifting away from `<input>` the way a copy would.

// ── autocomplete ────────────────────────────────────────────────────────────────────────────────

/// The "autofill field names" of HTML §4.10.18.7, split by the grammar position they occupy.
const AUTOFILL_CONTACT: [&str; 10] = [
    "tel", "tel-country-code", "tel-national", "tel-area-code", "tel-local", "tel-local-prefix",
    "tel-local-suffix", "tel-extension", "email", "impp",
];
const AUTOFILL_NORMAL: [&str; 44] = [
    "name", "honorific-prefix", "given-name", "additional-name", "family-name", "honorific-suffix",
    "nickname", "username", "new-password", "current-password", "one-time-code",
    "organization-title", "organization", "street-address", "address-line1", "address-line2",
    "address-line3", "address-level4", "address-level3", "address-level2", "address-level1",
    "country", "country-name", "postal-code", "cc-name", "cc-given-name", "cc-additional-name",
    "cc-family-name", "cc-number", "cc-exp", "cc-exp-month", "cc-exp-year", "cc-csc", "cc-type",
    "transaction-currency", "transaction-amount", "language", "bday", "bday-day", "bday-month",
    "bday-year", "sex", "url", "photo",
];

/// `autocomplete` — `[ReflectSetter]`, so the SETTER reflects verbatim but the GETTER returns the
/// element's "IDL-exposed autofill value", which is "" unless the attribute's tokens parse as an
/// autofill detail-tokens list. That asymmetry is the whole member: `input.autocomplete = "x"` stores
/// `autocomplete="x"` and still reads back "", because "x" is not an autofill field name.
fn autofill_value(raw: &str) -> String {
    let tokens: Vec<String> = raw.split_ascii_whitespace().map(|t| t.to_ascii_lowercase()).collect();
    if tokens.is_empty() || tokens.len() > 5 { return String::new(); }
    if tokens.len() == 1 && (tokens[0] == "on" || tokens[0] == "off") { return tokens[0].clone(); }
    let mut i = 0;
    // optional "section-*"
    if tokens[i].starts_with("section-") && tokens[i].len() > "section-".len() { i += 1; }
    // optional shipping/billing
    if i < tokens.len() && matches!(tokens[i].as_str(), "shipping" | "billing") { i += 1; }
    // optional contact modifier, valid only in front of a CONTACT field name
    let mut had_modifier = false;
    if i < tokens.len() && matches!(tokens[i].as_str(), "home" | "work" | "mobile" | "fax" | "pager") {
        had_modifier = true;
        i += 1;
    }
    let Some(field) = tokens.get(i) else { return String::new() };
    let is_contact = AUTOFILL_CONTACT.contains(&field.as_str());
    if had_modifier && !is_contact { return String::new(); }
    if !is_contact && !AUTOFILL_NORMAL.contains(&field.as_str()) { return String::new(); }
    i += 1;
    // optional trailing "webauthn" credential-type token
    if i < tokens.len() && tokens[i] == "webauthn" { i += 1; }
    if i != tokens.len() { return String::new(); }
    tokens.join(" ")
}
pub(crate) fn cb_input_autocomplete_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let raw = attr(scope, args.this(), "autocomplete").unwrap_or_default();
    let v = str_val(scope, &autofill_value(&raw));
    rv.set(v);
}

// ---- `value` where it is NOT the plain reflection (W5.0d: the port of dom/03-element-glue.js's value bridge helpers) ----
//
// One body, dispatched on the tag, exactly as the JS helper was and as dom/08l-input-element.js and
// dom/08b-reflected.js mount it: `<select>` answers the selected option's value (its `value` attribute,
// else its text; the first option when none is selected), `<textarea>` its `value` attribute else its
// text, and `<input>`/`<progress>`/`<meter>` the `value` content attribute ("" when absent). The setter
// writes the attribute through the native choke point; on a `<select>` it selects the option whose
// value equals the string. The per-type dirty-value rules of `<input>` are the HTMLInputElement
// table's (W5.1 L1), not this helper's.
pub(crate) fn cb_control_value_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = unwrap_id(scope, args.this().into()) else {
        let e = str_val(scope, "");
        rv.set(e);
        return;
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let tag = op_state.borrow().borrow::<crate::binding::host_dom::HostDom>().0.tag_name(id).map(|t| t.to_ascii_uppercase()).unwrap_or_default();
    // The arena's one answer: the value script assigned, else the markup's -- the
    // selected option's for a `<select>`, the child text for a `<textarea>`, the `value` attribute for
    // everything else. `tag` is read only for the doc comment's sake.
    let _ = tag;
    let s = op_state.borrow().borrow::<crate::binding::host_dom::HostDom>().0.control_value(id);
    let v = str_val(scope, &s);
    rv.set(v);
}
pub(crate) fn cb_control_value_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = unwrap_id(scope, args.this().into()) else { return };
    let v = args.get(0).to_rust_string_lossy(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let is_select = op_state.borrow().borrow::<crate::binding::host_dom::HostDom>().0.tag_name(id).map(|t| t.eq_ignore_ascii_case("select")).unwrap_or(false);
    // A `<select>`: the first option whose value is `v` becomes the selection, every other option
    // goes off (none on when no option has that value) -- selectedness, not attributes.
    if is_select {
        let mut st = op_state.borrow_mut();
        let dom = &mut st.borrow_mut::<crate::binding::host_dom::HostDom>().0;
        let options = dom.list_of_options(id);
        let target = options.iter().position(|&o| dom.option_value(o) == v).map_or(-1, |i| i as i32);
        dom.set_selected_index(id, target);
        return;
    }
    // NOT the content attribute: the API value and its dirty flag (Chrome leaves `defaultValue`
    // alone, which is what makes a step base and a form reset mean anything).
    let changed = {
        let mut st = op_state.borrow_mut();
        let dom = &mut st.borrow_mut::<crate::binding::host_dom::HostDom>().0;
        let changed = dom.control_value(id) != v;
        dom.set_control_value(id, &v);
        changed
    };
    // "If the new value is different: move the text entry cursor position to the end of the text
    // control and unselect any selected text" -- and only then. WPT's `selection-after-content-change`
    // asserts a `backward` 1..3 selection SURVIVES `element.value = <the same value>`, and
    // `selection.html` reads `selectionStart` right after a first assignment and expects the length.
    if changed {
        let end = v.encode_utf16().count() as u32;
        let this = args.this();
        set_slot_u32(scope, this, SEL_START_PRIV, end);
        set_slot_u32(scope, this, SEL_END_PRIV, end);
        let dir = str_val(scope, "forward");
        priv_set(scope, this, SEL_DIR_PRIV, dir);
    }
}
// `op_control_value_get` and `op_control_value_set`, the JS-facing builders for the pair above, ARE
// GONE. The callbacks are the shared body of `value` for THREE interfaces and are named
// directly by all three: `HTMLInputElement` (below), `HTMLSelectElement` (`pair_row`, both mounts)
// and `HTMLTextAreaElement` (`acc2`). The ops wrapped them and nothing called the wrappers. This file
// declares no ops.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_days_round_trip() {
        for (y, m, d) in [(1970, 1, 1), (2000, 2, 29), (1999, 12, 31), (2024, 6, 15), (1901, 3, 1)] {
            assert_eq!(civil_from_days(days_from_civil(y, m, d)), (y, m, d));
        }
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }

    #[test]
    fn date_values_convert() {
        assert_eq!(value_to_number("date", "1970-01-02"), 86_400_000.0);
        assert!(value_to_number("date", "2021-02-30").is_nan());
        assert!(value_to_number("date", "").is_nan());
        assert_eq!(value_to_number("month", "1970-03"), 2.0);
        assert_eq!(value_to_number("time", "00:01"), 60_000.0);
        assert_eq!(value_to_number("time", "12:30:45"), 45_045_000.0);
        assert!(value_to_number("time", "24:00").is_nan());
    }

    #[test]
    fn week_round_trips() {
        // 2004-W53 is a real ISO week (2004 was a 53-week year); 1970-01-01 was a Thursday, so
        // 1970-W01 starts on Monday 1969-12-29.
        assert_eq!(format_week(parse_week_ms("2004-W53").unwrap()).as_deref(), Some("2004-W53"));
        assert_eq!(format_week(parse_week_ms("1970-W01").unwrap()).as_deref(), Some("1970-W01"));
        assert_eq!(parse_week_ms("1970-W01"), Some(-3.0 * 86_400_000.0));
    }

    #[test]
    fn html_numbers_are_not_js_numbers() {
        assert!(parse_html_number("").is_nan());
        assert!(parse_html_number("0x10").is_nan());
        assert!(parse_html_number("Infinity").is_nan());
        assert_eq!(parse_html_number("-1.5"), -1.5);
    }

    #[test]
    fn autofill_value_limits_to_the_grammar() {
        assert_eq!(autofill_value("x"), "");
        assert_eq!(autofill_value(""), "");
        assert_eq!(autofill_value("on"), "on");
        assert_eq!(autofill_value("OFF"), "off");
        assert_eq!(autofill_value("Given-Name"), "given-name");
        assert_eq!(autofill_value("shipping tel"), "shipping tel");
        assert_eq!(autofill_value("section-a billing work email"), "section-a billing work email");
        // A contact modifier in front of a non-contact field is not the grammar.
        assert_eq!(autofill_value("work given-name"), "");
        assert_eq!(autofill_value("username webauthn"), "username webauthn");
    }

    #[test]
    fn emails_follow_the_html_abnf() {
        assert!(valid_email("a@b.co"));
        assert!(valid_email("a.b+c@example.museum"));
        assert!(!valid_email("a@"));
        assert!(!valid_email("@b.co"));
        assert!(!valid_email("a@-b.co"));
        assert!(!valid_email("a..b@c.co"));
    }
}

// ── the table ───────────────────────────────────────────────────────────────
//
// `dom/08l-input-element.js` (146 lines, three `Object.defineProperty` shapes over 59 members) is
// gone. Every one of its members is a row below, mounted by THE installer from this table
// (`install::live::install_window_small_elements`). Names, order, kinds, lengths, `required` counts, nullability
// and descriptor flags are VERBATIM from the `HTMLInputElement` block of
// `install/generated_registry.rs` -- the same order the bundle's `surface.json` records. Only the
// BODIES are this file's: the `= reflect_*` rows are the kits the generated rows already named, and
// every other row names one of the callbacks above, which the deleted JS reached through an `op_*`
// builder.
//
// FIVE ROWS DIFFER FROM THE GENERATED CFG, each because webref's input is thinner than the reflection
// actually is, and each matching what the deleted JS installed:
//
//   * `size` is the one reflected `unsigned long` in HTML whose missing value is not 0 -- an <input>
//     with no `size` attribute is 20 characters wide, so the cfg is `size|20`, not `size|0|`.
//   * `type`, `formEnctype`, `formMethod` and `popoverTargetAction` are ENUMERATED reflections
//     ("limited to only known values"): the getter clamps to the keyword set and answers the missing/
//     invalid default. webref carries no `[ReflectOnly]` for them, so `gen_reflected.py` had nothing
//     to emit and left them `stub`; the deleted JS installed the enum getter by hand.
//
// `autocomplete` is `[ReflectSetter]`: the setter reflects verbatim, the getter answers the element's
// IDL-exposed AUTOFILL value ("" for anything that is not an autofill field name).

/// Build one native function for a row, with the per-member `data` the shared callbacks read
/// (`cb_input_dim_get`'s "height", `cb_input_selection_get`'s "start", a reflect setter's attribute).
pub(crate) fn fn_of<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
    name: &str,
    len: i32,
    data: Option<&str>,
) -> v8::Local<'s, v8::Function> {
    match data {
        Some(d) => {
            let dv = str_val(scope, d);
            build_fn_data(scope, cb, name, len, dv)
        }
        None => build_fn(scope, cb, name, len),
    }
}

/// A readonly attribute row: `get <member>`, length 0.
pub(crate) fn acc1<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    proto: v8::Local<v8::Object>,
    m: &'static crate::install::MemberDef,
    gcb: impl v8::MapFnTo<v8::FunctionCallback>,
    gdata: Option<&str>,
) {
    let g = fn_of(scope, gcb, &format!("get {}", m.name), 0, gdata);
    crate::install::kits::define_accessor(scope, proto, m, g, None);
}

/// A read/write attribute row whose getter and setter are different callbacks, each with its own data.
pub(crate) fn acc2<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    proto: v8::Local<v8::Object>,
    m: &'static crate::install::MemberDef,
    gcb: impl v8::MapFnTo<v8::FunctionCallback>,
    gdata: Option<&str>,
    scb: impl v8::MapFnTo<v8::FunctionCallback>,
    sdata: Option<&str>,
) {
    let g = fn_of(scope, gcb, &format!("get {}", m.name), 0, gdata);
    let s = fn_of(scope, scb, &format!("set {}", m.name), 1, sdata);
    crate::install::kits::define_accessor(scope, proto, m, g, Some(s));
}

/// The constraint-validation + labels members `<input>`, `<button>`, `<object>` and `<textarea>`
/// share. One definition, four callers -- the neutral-named callbacks branch on the element, so a
/// `<button>` gets the `<button>` rules out of the same body.
pub(crate) fn install_form_control_shared(
    scope: &mut v8::PinScope,
    proto: v8::Local<v8::Object>,
    member: &'static crate::install::MemberDef,
) -> bool {
    use crate::install::live::method_row;
    match member.name {
        "form" => acc1(scope, proto, member, cb_input_form_get, None),
        "willValidate" => acc1(scope, proto, member, cb_input_will_validate, None),
        "validity" => acc1(scope, proto, member, cb_input_validity, None),
        "validationMessage" => acc1(scope, proto, member, cb_input_validation_message, None),
        "labels" => acc1(scope, proto, member, cb_input_labels_get, None),
        // reportValidity differs from checkValidity only in also showing the browser's validation
        // bubble, and there is no widget layer here -- the same callback, as in the deleted JS.
        "checkValidity" | "reportValidity" => method_row(scope, proto, member, cb_input_check_validity),
        "setCustomValidity" => method_row(scope, proto, member, cb_input_set_custom_validity),
        _ => return false,
    }
    true
}

/// The text-selection mixin `<input>` and `<textarea>` share (`input_type()` reports "text" for
/// anything that is not an `<input>`, which is the branch a textarea wants).
pub(crate) fn install_selection_shared(
    scope: &mut v8::PinScope,
    proto: v8::Local<v8::Object>,
    member: &'static crate::install::MemberDef,
) -> bool {
    use crate::install::live::method_row;
    match member.name {
        "selectionStart" => acc2(scope, proto, member, cb_input_selection_get, Some("start"), cb_input_selection_set, Some("start")),
        "selectionEnd" => acc2(scope, proto, member, cb_input_selection_get, Some("end"), cb_input_selection_set, Some("end")),
        "selectionDirection" => acc2(scope, proto, member, cb_input_selection_get, Some("direction"), cb_input_selection_set, Some("direction")),
        "select" => method_row(scope, proto, member, cb_input_select),
        "setRangeText" => method_row(scope, proto, member, cb_input_set_range_text),
        "setSelectionRange" => method_row(scope, proto, member, cb_input_set_selection_range),
        "createValueRange" => method_row(scope, proto, member, cb_create_value_range),
        _ => return false,
    }
    true
}

pub fn install_html_input_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static crate::install::MemberDef) {
    use crate::core::dom::element::cb_reflect_str_set;
    use crate::install::kits::define_value;
    use crate::install::live::method_row;
    if install_form_control_shared(scope, proto, member) || install_selection_shared(scope, proto, member) {
        return;
    }
    match member.name {
        "autocomplete" => acc2(scope, proto, member, cb_input_autocomplete_get, None, cb_reflect_str_set, Some("autocomplete")),
        // The CHECKEDNESS, not the attribute: once assigned it stops tracking `defaultChecked`,
        // which is what makes a form reset meaningful.
        "checked" => acc2(scope, proto, member, cb_input_checked_get, None, cb_input_checked_set, None),
        "files" => acc2(scope, proto, member, cb_input_files_get, None, cb_input_files_set, None),
        "height" => acc2(scope, proto, member, cb_input_dim_get, Some("height"), cb_input_dim_set, Some("height")),
        "width" => acc2(scope, proto, member, cb_input_dim_get, Some("width"), cb_input_dim_set, Some("width")),
        "indeterminate" => acc2(scope, proto, member, cb_input_indeterminate_get, None, cb_input_indeterminate_set, None),
        "list" => acc1(scope, proto, member, cb_input_list_get, None),
        "value" => acc2(scope, proto, member, cb_control_value_get, None, cb_control_value_set, None),
        "valueAsDate" => acc2(scope, proto, member, cb_input_value_as_date_get, None, cb_input_value_as_date_set, None),
        "valueAsNumber" => acc2(scope, proto, member, cb_input_value_as_number_get, None, cb_input_value_as_number_set, None),
        "webkitEntries" => acc1(scope, proto, member, cb_input_webkit_entries, None),
        "showPicker" => method_row(scope, proto, member, cb_input_show_picker),
        // `stepUp`/`stepDown` are ONE body with a sign in its data, which is how the two ops the
        // deleted JS named were built.
        "stepUp" | "stepDown" => {
            let sign = if member.name == "stepUp" { 1.0 } else { -1.0 };
            let d: v8::Local<v8::Value> = v8::Number::new(scope, sign).into();
            let f = build_fn_data(scope, cb_input_step_by, member.name, member.length as i32, d);
            define_value(scope, proto, member, f.into(), true);
        }
        other => debug_assert!(false, "install_html_input_element: unexpected member {other}"),
    }
}

webapi::interface! {
    "HTMLInputElement" : "HTMLElement" [element, window] {
        ctor   "HTMLInputElement"                 len 0                 = stub,
        attr   "accept"               : Object cfg "accept"             = reflect_str,
        attr   "alt"                  : Object cfg "alt"                = reflect_str,
        attr   "autocomplete"         : Object cfg "autocomplete"       = install_html_input_element,
        attr   "defaultChecked"       : Object cfg "checked"            = reflect_bool,
        attr   "checked"              : Object                          = install_html_input_element,
        attr   "dirName"              : Object cfg "dirname"            = reflect_str,
        attr   "disabled"             : Object cfg "disabled"           = reflect_bool,
        attr   "form"                 : Object readonly nullable        = install_html_input_element,
        attr   "files"                : Object nullable                 = install_html_input_element,
        attr   "formAction"           : Object cfg "formaction"         = reflect_str,
        attr   "formEnctype"          : Object cfg "formenctype|application/x-www-form-urlencoded,multipart/form-data,text/plain||application/x-www-form-urlencoded" = reflect_enum,
        attr   "formMethod"           : Object cfg "formmethod|get,post,dialog||get" = reflect_enum,
        attr   "formNoValidate"       : Object cfg "formnovalidate"     = reflect_bool,
        attr   "formTarget"           : Object cfg "formtarget"         = reflect_str,
        attr   "height"               : Object                          = install_html_input_element,
        attr   "indeterminate"        : Object                          = install_html_input_element,
        attr   "list"                 : Object readonly nullable        = install_html_input_element,
        attr   "max"                  : Object cfg "max"                = reflect_str,
        attr   "maxLength"            : Object cfg "maxlength|-1|nonneg" = reflect_long,
        attr   "min"                  : Object cfg "min"                = reflect_str,
        attr   "minLength"            : Object cfg "minlength|-1|nonneg" = reflect_long,
        attr   "multiple"             : Object cfg "multiple"           = reflect_bool,
        attr   "name"                 : Object cfg "name"               = reflect_str,
        attr   "pattern"              : Object cfg "pattern"            = reflect_str,
        attr   "placeholder"          : Object cfg "placeholder"        = reflect_str,
        attr   "readOnly"             : Object cfg "readonly"           = reflect_bool,
        attr   "required"             : Object cfg "required"           = reflect_bool,
        attr   "size"                 : Object cfg "size|20"            = reflect_ulong,
        attr   "src"                  : Object cfg "src"                = reflect_url,
        attr   "step"                 : Object cfg "step"               = reflect_str,
        attr   "type"                 : Object cfg "type|hidden,text,search,tel,url,email,password,date,month,week,time,datetime-local,number,range,color,checkbox,radio,file,submit,image,reset,button|text|text" = reflect_enum,
        attr   "defaultValue"         : Object cfg "value"              = reflect_str,
        attr   "value"                : Object                          = install_html_input_element,
        attr   "valueAsDate"          : Object nullable                 = install_html_input_element,
        attr   "valueAsNumber"        : Object                          = install_html_input_element,
        attr   "width"                : Object                          = install_html_input_element,
        attr   "willValidate"         : Object readonly                 = install_html_input_element,
        attr   "validity"             : Object readonly                 = install_html_input_element,
        attr   "validationMessage"    : Object readonly                 = install_html_input_element,
        attr   "labels"               : Object readonly nullable        = install_html_input_element,
        attr   "selectionStart"       : Object nullable                 = install_html_input_element,
        attr   "selectionEnd"         : Object nullable                 = install_html_input_element,
        attr   "selectionDirection"   : Object nullable                 = install_html_input_element,
        attr   "align"                : Object cfg "align"              = reflect_str,
        attr   "useMap"               : Object cfg "usemap"             = reflect_str,
        attr   "webkitdirectory"      : Object cfg "webkitdirectory"    = reflect_bool,
        attr   "incremental"          : Object cfg "incremental"        = reflect_bool,
        attr   "popoverTargetElement" : Object nullable cfg "popovertarget" = reflect_element,
        attr   "popoverTargetAction"  : Object cfg "popovertargetaction|toggle,show,hide|toggle|toggle" = reflect_enum,
        method "checkValidity"        : Object len 0                    = install_html_input_element,
        method "reportValidity"       : Object len 0                    = install_html_input_element,
        method "select"               : Object len 0                    = install_html_input_element,
        method "setCustomValidity"    : Object len 1 required 1         = install_html_input_element,
        method "setRangeText"         : Object len 1 required 1         = install_html_input_element,
        method "setSelectionRange"    : Object len 2 required 2         = install_html_input_element,
        method "showPicker"           : Object len 0                    = install_html_input_element,
        method "stepDown"             : Object len 0                    = install_html_input_element,
        method "stepUp"               : Object len 0                    = install_html_input_element,
        attr   "webkitEntries"        : Object readonly                 = install_html_input_element,
        // Chrome 152 (`opaque_range.idl`, `HTMLInputElement includes OpaqueRangeCreation`), last on the prototype.
        method "createValueRange"     : Object len 2 required 2         = install_html_input_element
    }
}
