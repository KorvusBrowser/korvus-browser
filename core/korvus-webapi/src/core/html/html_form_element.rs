//! `core/html/html_form_element.rs` -- `HTMLFormElement` as one `interface!` table.
//!
//! `dom/08w-form-element.js` (174 lines, six `KB.__form*` bridge slots, three `op_helper_*` native
//! masks, three `Object.defineProperty` sites) and the four `HTMLFormElement` lines of
//! `dom/08b-reflected.js` are gone. Every row below is mounted by THE installer
//! (`install::live::install_window_forms`), with names, order, kinds, lengths, `required` counts,
//! nullability and descriptor flags VERBATIM from the `HTMLFormElement` block of
//! `install/generated_registry.rs` (which is the capture's own order).
//!
//! THE FOUR ENUMERATED REFLECTIONS. `method`, `enctype`, `encoding` and `autocomplete` are ENUMERATED
//! reflections with NON-EMPTY defaults, which is why a plain string reflection is wrong for them: the
//! browser answers `"get"`, `"application/x-www-form-urlencoded"` and `"on"` where `reflect_str` would
//! answer `""`, and it answers the DEFAULT again for an unrecognised keyword (`form.method = "x"` reads
//! back `"get"`). `encoding` is not a second attribute: it is a second IDL name for `enctype`, so both
//! rows carry the same `cfg` and read and write the same content attribute.
//!
//! `action` IS NOT A PLAIN `reflect_url`. HTML §4.10.3: "The action IDL attribute must reflect the
//! content attribute of the same name, except that on getting, when the content attribute is missing or
//! its value is the empty string, the element's node document's URL must be returned instead." The
//! shared `reflect_url` kit answers `""` for an ABSENT attribute (which is right for `<link href>` and
//! every other URL reflection) so `action` gets its own body -- the one row of this table with a
//! deliberate divergence from the kit its registry row named. Chrome 151, measured 2026-09-11 over
//! loopback: a fresh `<form>` reads the document's URL, `action="/x"` reads the absolute resolution and
//! `action=""` reads the document's URL again.
//!
//! FORM SUBMISSION, WHOLE. Until P13c this file ran the submission algorithm's DATA
//! path and stopped: it could navigate a NAMED FRAME (and only by faking it -- the frame's location
//! moved and no request went out), a POST carried no body, `requestSubmit` and `reset` were D2 stubs,
//! neither the `submit` nor the `formdata` event existed, and a form whose target was the PAGE -- the
//! default, and how every search box and filter UI on the web moves -- did nothing at all. What runs
//! now is the algorithm: interactive validation, the `submit` event with its submitter, the entry
//! list, the `formdata` event whose `FormData` a listener may still change, the three encodings, the
//! `formaction`/`formmethod`/`formenctype`/`formtarget`/`formnovalidate` overrides, `method=dialog`,
//! and "choose a browsing context" -- which reaches the top-level navigation queue P12d built.
//!
//! What is still NOT here, named so nobody infers otherwise from a passing test:
//!   * a target that names NO existing context (`_blank`, an unknown name) navigates nothing. Chrome
//!     creates an auxiliary context only with transient user activation, which this engine has no
//!     model of at all (`UserActivation.isActive` is a D2 stub answering false), so a blanket "open
//!     one" would be as wrong as the blanket "open none" and louder. Measured: Chrome 150 headless,
//!     `<form target=_blank>` submitted from `onload`, navigates nothing and requests nothing --
//!     which is what this does.
//!   * `reset()` stays a D2 stub: it needs the form-reset algorithm (every control's
//!     `defaultValue`/`defaultChecked` restore plus a `reset` event), which is the form CONTROLS'
//!     unit, not the submission path's.
//!   * `new SubmitEvent("submit", {submitter: b}).submitter` is still `null`: the interface's
//!     constructor is the registry's D2 shell one (the interface stays a registry row because its
//!     global sits in the capture's own-global order). The ENGINE-minted event carries the real
//!     submitter, which is what a page's `submit` listener reads.
//!
//! RECEIVER CHECKS. Every body written here rejects a receiver that is not a `<form>` with Chrome's
//! `TypeError: Illegal invocation` (measured in Chrome 151:
//! `HTMLFormElement.prototype.__lookupGetter__("length").call({})` throws exactly that). The shared
//! `reflect_*` kits answer as they do on every other interface -- that is engine-wide behaviour this
//! lane does not change.

use deno_core::v8;

use crate::binding::protos::{get_prop, pristine_call, pristine_get, str_val, throw_dom, throw_type_error};
use crate::core::dom::collections::{make_live, Kind};
use crate::core::dom::element::el_id_of;
use crate::core::events::event::evt_slot_write;
use crate::core::events::event_target::et_dispatch;
use crate::core::html::browsing_context::{
    choose_target, context_of_element, element_document_is_current, navigate_target, NavigationReason, PageNavigation,
};
use crate::core::html::form_encoding::urlencode;
use crate::install::live::{getter_row, method_row};
use crate::install::MemberDef;

// ── the receiver check ──────────────────────────────────────────────────────────────────────────

/// The receiver's arena id when it is a `<form>`; otherwise Chrome's `Illegal invocation` TypeError and
/// `None`. `tagName` rather than the arena's tag because that is what the deleted JS compared and it is
/// what an element built by any of Korvus's mint paths answers.
fn this_form(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> Option<u32> {
    let this = args.this();
    let is_form = crate::page::tag_name_of(scope, (this).into()).is_some_and(|t| t.eq_ignore_ascii_case("form"));
    let id = if is_form { el_id_of(scope, this) } else { None };
    if id.is_none() {
        throw_type_error(scope, "Illegal invocation");
    }
    id
}

// ── the document's URL and encoding ─────────────────────────────────────────────────────────────

/// The document's URL -- `CookieUrl`. `<form action>` is the one URL reflection on the page that still
/// needs it: HTML says a MISSING or EMPTY `action` reports "the element's node document's URL", and
/// Chrome confirms the distinction is real -- with `<base href="/sub/">` on `/base3.html`,
/// `action=""` reads `/base3.html` while `action="go"` reads `/sub/go`.
fn document_url(scope: &mut v8::PinScope) -> String {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    crate::core::dom::document_url_of(&st)
}

/// The document's BASE url -- what a NON-empty relative `action` resolves against, like every other
/// URL on the page (`core/dom/document.rs::document_base_url` owns the rule).
fn document_base(scope: &mut v8::PinScope) -> String {
    crate::core::dom::base_url(scope)
}

/// The document's declared encoding label -- what `document.characterSet` reports (`DocEncoding`,
/// resolved by the parser from `<meta charset>`).
fn document_charset(scope: &mut v8::PinScope) -> String {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<crate::core::dom::DocEncoding>().map(|d| d.0.name()).unwrap_or("UTF-8").to_string()
}

/// The interface a listed control's own members are on, by its tag -- the members the form's steps run on it
/// are the realm's ORIGINAL ones (`protos::pristine_call`), never the page's: Blink's form algorithms read and
/// validate each control in C++, and these called its `checkValidity`, `value`, `checked`, `form` as a page
/// would (`tests/engine_reads_live.rs`). `None` for a form-associated custom element,
/// whose members are its author's.
fn control_iface(scope: &mut v8::PinScope, el: v8::Local<v8::Object>) -> Option<&'static str> {
    let tag = crate::page::tag_name_of(scope, el.into())?.to_ascii_lowercase();
    Some(match tag.as_str() {
        "input" => "HTMLInputElement",
        "button" => "HTMLButtonElement",
        "select" => "HTMLSelectElement",
        "textarea" => "HTMLTextAreaElement",
        "object" => "HTMLObjectElement",
        "output" => "HTMLOutputElement",
        "fieldset" => "HTMLFieldSetElement",
        _ => return None,
    })
}

/// A listed control's `checkValidity()` -- `None` when it has none of the realm's own.
fn control_check_validity(scope: &mut v8::PinScope, c: v8::Local<v8::Object>) -> Option<bool> {
    let iface = control_iface(scope, c)?;
    let r = pristine_call(scope, c.into(), &format!("{iface}.checkValidity"), &[])?;
    Some(r.boolean_value(scope))
}

/// A submit button's form owner (an `<input>` or a `<button>`).
fn control_form<'s>(scope: &mut v8::PinScope<'s, '_>, el: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::Value>> {
    let iface = control_iface(scope, el)?;
    pristine_get(scope, el.into(), &format!("{iface}.form")).filter(|v| v.is_object())
}

fn attr(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, name: &str) -> Option<String> {
    let a = str_val(scope, name);
    match crate::core::dom::element::attr_call(scope, this.into(), "getAttribute", &[a]) {
        Some(v) if !v.is_null_or_undefined() => Some(v.to_rust_string_lossy(scope)),
        _ => None,
    }
}

// ── the rows ────────────────────────────────────────────────────────────────────────────────────

/// `action` -- the URL reflection with the document-URL fallback (see the file comment).
fn cb_form_action_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if this_form(scope, &args).is_none() {
        return;
    }
    let raw = attr(scope, args.this(), "action").unwrap_or_default();
    if raw.is_empty() {
        // Missing or empty: the DOCUMENT url, which `<base href>` does not move.
        let doc = document_url(scope);
        let v = str_val(scope, &doc);
        rv.set(v);
        return;
    }
    let base = document_base(scope);
    let parsed = url::Url::parse(&raw).ok().or_else(|| url::Url::parse(&base).ok().and_then(|b| b.join(&raw).ok()));
    let out = match parsed {
        Some(u) => u.to_string(),
        // A document with no parseable base leaves a relative reference as written -- the same
        // fallback `cb_reflect_url_get` takes.
        None => raw,
    };
    let v = str_val(scope, &out);
    rv.set(v);
}

/// `elements` -- the live `HTMLFormControlsCollection` of the form's listed controls (`Kind::FormControls`,
/// the collection `core/dom/collections.rs` already mints; this row does not build a second list).
fn cb_form_elements_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = this_form(scope, &args) else { return };
    let list = make_live(scope, "HTMLFormControlsCollection", id, Kind::FormControls, "", None);
    rv.set(list);
}

/// `length` -- `elements.length`, read off the same live collection so the two can never disagree.
fn cb_form_length_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = this_form(scope, &args) else { return };
    let list = make_live(scope, "HTMLFormControlsCollection", id, Kind::FormControls, "", None);
    let n = pristine_get(scope, list, "HTMLCollection.length").and_then(|v| v.uint32_value(scope)).unwrap_or(0);
    rv.set_uint32(n);
}

/// The form's listed controls as a Vec, in tree order.
fn form_controls<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32) -> Vec<v8::Local<'s, v8::Object>> {
    let list = make_live(scope, "HTMLFormControlsCollection", id, Kind::FormControls, "", None);
    let Ok(obj) = v8::Local::<v8::Object>::try_from(list) else { return Vec::new() };
    let n = pristine_get(scope, list, "HTMLCollection.length").and_then(|v| v.uint32_value(scope)).unwrap_or(0);
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n {
        if let Some(v) = obj.get_index(scope, i) {
            if let Ok(o) = v8::Local::<v8::Object>::try_from(v) {
                out.push(o);
            }
        }
    }
    out
}

/// `checkValidity()` / `reportValidity()` -- "statically validate the constraints" over every listed
/// control, in tree order, each one reporting through its OWN `checkValidity` so that the `invalid`
/// events fire where the spec puts them. This deliberately does not short-circuit: `every` would skip
/// the later controls' events. A form with no controls is valid, which is what the recording shows.
/// `reportValidity` differs only by showing the browser's validation bubble, which Korvus has no UI for
/// -- the same reason `<input>` and `<button>` share one callback for the pair.
fn cb_form_check_validity(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = this_form(scope, &args) else { return };
    let mut ok = true;
    for c in form_controls(scope, id) {
        if control_check_validity(scope, c) == Some(false) {
            ok = false;
        }
    }
    rv.set_bool(ok);
}

/// The submission encoding: the form's `accept-charset`, else the document's. `accept-charset` is an
/// ordered set of labels and the first SUPPORTED one wins -- a form may name three encodings and mean
/// "whichever of these you have". An unrecognised label falls through to [`urlencode`], which falls back
/// to UTF-8 exactly as the spec asks.
fn submission_charset(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> String {
    if let Some(ac) = attr(scope, this, "accept-charset") {
        if let Some(first) = ac.split(|c: char| c.is_ascii_whitespace() || c == ',').find(|s| !s.is_empty()) {
            return first.to_string();
        }
    }
    // The FORM's document's -- a form in a frame submits in the frame's encoding.
    match crate::binding::protos::arg_node_id(scope, this.into()) {
        Some(id) => crate::core::html::browsing_context::node_document_encoding(scope, id).name().to_string(),
        None => document_charset(scope),
    }
}

/// One entry of the form's entry list. `file` marks an entry that came from a file control: its part
/// of a `multipart/form-data` body carries a `filename` and a `Content-Type` where a text entry does
/// not.
struct Entry {
    name: String,
    value: String,
    file: bool,
    /// The file a file control's entry carries (`None` for an empty selection's one entry, and for every other
    /// control).
    part: Option<crate::core::html::form_encoding::FilePart>,
}

/// The entry list ("construct the entry list"): every submittable control with a non-empty name that
/// is not disabled, in tree order. Checkboxes and radios contribute only when checked, and contribute
/// `"on"` when they carry no `value` attribute; a button or a `submit`/`image` input contributes ONLY
/// when it is the SUBMITTER, which is why `submitter` is a parameter and not a lookup -- `submit()`
/// has none and a button click has exactly one.
///
/// An `image` submitter contributes its coordinates rather than its value (`name.x`/`name.y`, or plain
/// `x`/`y` when it has no name), which is the one control whose entry is not `(name, value)`.
fn entry_list(scope: &mut v8::PinScope, id: u32, submitter: Option<v8::Local<v8::Object>>) -> Vec<Entry> {
    let mut out = Vec::new();
    for el in form_controls(scope, id) {
        let is_submitter = submitter.map(|s| s == el).unwrap_or(false);
        // `disabled` reflects the content attribute: read it off the element, not through the getter
        if attr(scope, el, "disabled").is_some() {
            continue;
        }
        let tag = crate::page::tag_name_of(scope, (el).into()).unwrap_or_default();
        if tag.eq_ignore_ascii_case("fieldset") || tag.eq_ignore_ascii_case("output") {
            continue;
        }
        let ty = if tag.eq_ignore_ascii_case("input") {
            attr(scope, el, "type").unwrap_or_else(|| "text".to_string()).to_ascii_lowercase()
        } else if tag.eq_ignore_ascii_case("button") {
            attr(scope, el, "type").unwrap_or_else(|| "submit".to_string()).to_ascii_lowercase()
        } else {
            String::new()
        };
        if tag.eq_ignore_ascii_case("button") || matches!(ty.as_str(), "submit" | "image") {
            if !is_submitter {
                continue;
            }
        }
        if matches!(ty.as_str(), "reset" | "button") {
            continue;
        }
        let name = attr(scope, el, "name").unwrap_or_default();
        if ty == "image" {
            // The coordinates of the click. A script-driven activation has none, so both are 0 --
            // which is what Chrome sends for `input.click()` on an image button (measured).
            let (nx, ny) =
                if name.is_empty() { ("x".to_string(), "y".to_string()) } else { (format!("{name}.x"), format!("{name}.y")) };
            out.push(Entry { name: nx, value: "0".to_string(), file: false, part: None });
            out.push(Entry { name: ny, value: "0".to_string(), file: false, part: None });
            continue;
        }
        if name.is_empty() {
            continue;
        }
        if matches!(ty.as_str(), "checkbox" | "radio") {
            if !pristine_get(scope, el.into(), "HTMLInputElement.checked").is_some_and(|v| v.boolean_value(scope)) {
                continue;
            }
            if attr(scope, el, "value").is_none() {
                out.push(Entry { name, value: "on".to_string(), file: false, part: None });
                continue;
            }
        }
        // A file control contributes one entry per selected file -- its name as the value, the file as the part -- and
        // one entry with no file when nothing is selected (HTML "construct the entry list").
        if ty == "file" {
            let files = crate::core::html::html_input_element::selected_files(scope, el);
            if files.is_empty() {
                out.push(Entry { name: name.clone(), value: String::new(), file: true, part: None });
            }
            for f in files {
                out.push(Entry { name: name.clone(), value: f.name.clone(), file: true, part: Some(f) });
            }
            continue;
        }
        let value = match control_iface(scope, el) {
            Some(iface @ ("HTMLInputElement" | "HTMLButtonElement" | "HTMLSelectElement" | "HTMLTextAreaElement")) => pristine_get(scope, el.into(), &format!("{iface}.value")),
            Some(_) => None,
            None => get_prop(scope, el, "value"),
        };
        let value = match value {
            Some(v) if !v.is_null_or_undefined() => v.to_rust_string_lossy(scope),
            _ => String::new(),
        };
        out.push(Entry { name, value, file: false, part: None });
    }
    out
}

/// The `formdata` event: a `FormDataEvent` carrying a `FormData` over the entry list, fired at the
/// form. A listener may add to or remove from that object, and what it leaves is what is submitted --
/// which is the whole point of the event, so the list is read BACK out of it afterwards.
///
/// Fired for `submit()` too: the method skips the `submit` event, not this one (measured, Chrome 150 --
/// a `form.submit()` with both listeners attached logs `formdata` and never `submit`).
fn fire_formdata<'s>(scope: &mut v8::PinScope<'s, '_>, form: v8::Local<'s, v8::Object>, entries: Vec<Entry>) -> Vec<Entry> {
    let Some(fd) = crate::binding::protos::new_global(scope, "FormData", &[]) else { return entries };
    let fdv: v8::Local<v8::Value> = fd.into();
    for e in &entries {
        let k = str_val(scope, &e.name);
        let v = str_val(scope, &e.value);
        pristine_call(scope, fdv, "FormData.append", &[k, v]);
    }
    // each file entry's part, by its (name, value), for the entries the listener leaves
    let mut files: std::collections::HashMap<(String, String), std::collections::VecDeque<Option<crate::core::html::form_encoding::FilePart>>> =
        std::collections::HashMap::new();
    for e in entries.iter().filter(|e| e.file) {
        files.entry((e.name.clone(), e.value.clone())).or_default().push_back(e.part.clone());
    }
    let Some(ev) = mint_event(scope, "FormDataEvent", "formdata", true, false) else { return entries };
    evt_slot_write(scope, ev, "formData", fdv);
    et_dispatch(scope, form.into(), ev);
    let Some(pairs) = crate::modules::fetch::form_data::fd_pairs(scope, fdv) else { return entries };
    pairs
        .into_iter()
        .map(|(name, value)| match files.get_mut(&(name.clone(), value.clone())).and_then(|q| q.pop_front()) {
            Some(part) => Entry { name, value, file: true, part },
            None => Entry { name, value, file: false, part: None },
        })
        .collect()
}

/// A trusted event of `iface`, minted on that interface's sealed prototype with the `Event` init steps
/// and the interface's own brand -- the two things its rows check. The interface objects themselves
/// are REGISTRY rows (they sit in the capture's own-global order), so there is no table constructor to
/// call; this is the same mint shape `core/dom/geometry_queries.rs` uses for `CaretPosition`.
fn mint_event<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    iface: &str,
    ty: &str,
    bubbles: bool,
    cancelable: bool,
) -> Option<v8::Local<'s, v8::Object>> {
    let proto = crate::binding::protos::shell_proto(scope, iface)?;
    // from the interface's template
    let ev = crate::binding::protos::mint_on(scope, proto.into());
    let dict = crate::binding::protos::init_dict(scope);
    crate::binding::protos::set_bool(scope, dict, "bubbles", bubbles);
    crate::binding::protos::set_bool(scope, dict, "cancelable", cancelable);
    crate::core::events::ctor::init_event(scope, ev, ty, Some(dict));
    crate::core::events::ctor::brand(scope, ev, iface);
    crate::core::html::html_dialog_element::mark_trusted(scope, ev);
    Some(ev)
}

/// An attribute of the SUBMITTER when it carries one, else the form's own -- the `formaction` /
/// `formmethod` / `formenctype` / `formtarget` / `formnovalidate` overrides.
fn submitter_or_form(
    scope: &mut v8::PinScope,
    form: v8::Local<v8::Object>,
    submitter: Option<v8::Local<v8::Object>>,
    on_submitter: &str,
    on_form: &str,
) -> Option<String> {
    if let Some(s) = submitter {
        if let Some(v) = attr(scope, s, on_submitter) {
            return Some(v);
        }
    }
    attr(scope, form, on_form)
}

/// HTML's "form submission algorithm".
///
/// `from_submit_method` is the spec's "submitted from `submit()` method" flag, and it is what
/// separates the two entry points: `form.submit()` runs NEITHER interactive validation NOR the
/// `submit` event, while `requestSubmit()` and a submit button's activation run both. Measured on
/// Chrome 150 over loopback (`plugins/korvus-cdp/tests/navigation.rs` carries the table):
///
/// ```text
/// <input required value="">   form.submit()        navigates to ?q=          no `invalid`
///                             form.requestSubmit() stays                     one `invalid`, :invalid x2
///                             button.click()       stays                     one `invalid`
///                             + novalidate         navigates to ?q=          no `invalid`
/// listener preventDefault()   form.submit()        navigates (no submit event fires at all)
///                             form.requestSubmit() stays
/// ```
pub(crate) fn submit_form<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    form: v8::Local<'s, v8::Object>,
    submitter: Option<v8::Local<'s, v8::Object>>,
    from_submit_method: bool,
) {
    let Some(id) = el_id_of(scope, form) else { return };
    // HTML "if form cannot navigate, then return": a form that is not CONNECTED, or whose node
    // document is no longer the one its browsing context is showing, submits nothing. Both are
    // measured -- `submission-checks.window.js` asserts the target frame stays on `about:blank` for
    // the first and on its own document for the second.
    if !can_navigate(scope, id) {
        return;
    }
    // "If form's constructing entry list is true, then return" -- a submission started from a
    // `formdata` listener, which runs inside the entry list's construction.
    if with_flags(scope, |f| f.constructing.contains(&id)) {
        return;
    }
    let no_validate = submitter
        .and_then(|s| attr(scope, s, "formnovalidate").map(|_| true))
        .unwrap_or_else(|| attr(scope, form, "novalidate").is_some());
    if !from_submit_method {
        // "If form's firing submission events is true, then return; otherwise set it to true."
        if with_flags(scope, |f| !f.firing.insert(id)) {
            return;
        }
        if !no_validate && !static_validation(scope, id) {
            with_flags(scope, |f| f.firing.remove(&id));
            return;
        }
        let Some(ev) = mint_event(scope, "SubmitEvent", "submit", true, true) else {
            with_flags(scope, |f| f.firing.remove(&id));
            return;
        };
        let s: v8::Local<v8::Value> = match submitter {
            Some(s) => s.into(),
            None => v8::null(scope).into(),
        };
        evt_slot_write(scope, ev, "submitter", s);
        let go = et_dispatch(scope, form.into(), ev);
        // "Set form's firing submission events to false" -- BEFORE the cancellation check, so a
        // cancelled submission leaves the form submittable again.
        with_flags(scope, |f| f.firing.remove(&id));
        if !go {
            return;
        }
        // The `submit` listener may have taken the form out of the document; HTML re-checks here, and
        // `submission-checks.window.js` asserts it ("not connected to a document AFTER submit event").
        if !can_navigate(scope, id) {
            return;
        }
    }
    // "Set form's constructing entry list to true" ... and false again once the list is built. The
    // `formdata` event is fired inside that window, which is what makes a submission started from a
    // `formdata` listener return at the check above.
    with_flags(scope, |f| f.constructing.insert(id));
    let raw = entry_list(scope, id, submitter);
    let entries = fire_formdata(scope, form, raw);
    with_flags(scope, |f| f.constructing.remove(&id));

    let method = submitter_or_form(scope, form, submitter, "formmethod", "method").unwrap_or_default().to_ascii_lowercase();
    // `method=dialog` closes the nearest ancestor dialog with the submitter's value and navigates
    // nothing (measured: `open` false, `returnValue` the button's value).
    if method == "dialog" {
        close_ancestor_dialog(scope, form, submitter);
        return;
    }
    let action = submitter_or_form(scope, form, submitter, "formaction", "action").unwrap_or_default();
    // A missing/empty action submits to the document's own URL, per the spec; anything else resolves
    // against the document's BASE url, like every other URL on the page.
    let base = if action.is_empty() { document_url(scope) } else { document_base(scope) };
    let target = if action.is_empty() { base.clone() } else { action };
    let Some(mut url) = url::Url::parse(&target).ok().or_else(|| url::Url::parse(&base).ok().and_then(|b| b.join(&target).ok()))
    else {
        return;
    };

    let charset = submission_charset(scope, form);
    let pairs: Vec<(String, String)> = entries.iter().map(|e| (e.name.clone(), e.value.clone())).collect();
    let nav = if method == "post" {
        let enctype = submitter_or_form(scope, form, submitter, "formenctype", "enctype").unwrap_or_default().to_ascii_lowercase();
        let (body, content_type) = match enctype.trim() {
            "multipart/form-data" => {
                let boundary = crate::core::html::form_encoding::multipart_boundary();
                // an empty selection's entry is a file part with no name and no bytes
                let files: Vec<Option<crate::core::html::form_encoding::FilePart>> =
                    entries.iter().map(|e| e.file.then(|| e.part.clone().unwrap_or_default())).collect();
                (
                    crate::core::html::form_encoding::multipart_bytes(&pairs, &files, &boundary),
                    format!("multipart/form-data; boundary={boundary}"),
                )
            }
            "text/plain" => (crate::core::html::form_encoding::text_plain_encode(&pairs).into_bytes(), "text/plain".to_string()),
            // Every other `enctype`, including an unrecognised one, is the urlencoded default.
            _ => (urlencode(&pairs, &charset).into_bytes(), "application/x-www-form-urlencoded".to_string()),
        };
        PageNavigation {
            method: "POST".to_string(),
            body: Some(body),
            content_type: Some(content_type),
            ..PageNavigation::get(url.to_string(), NavigationReason::FormSubmissionPost)
        }
    } else {
        // GET replaces the WHOLE query and keeps the fragment (measured: `action="echo?keep=1#h"`
        // submits `echo?q=1#h`). An `enctype` is ignored: a GET has no body to encode.
        let body = urlencode(&pairs, &charset);
        url.set_query(if body.is_empty() { None } else { Some(&body) });
        PageNavigation::get(url.to_string(), NavigationReason::FormSubmissionGet)
    };

    // The form's OWN document's context, not the realm whose script called `submit()`: a frame's
    // `onload` runs from the page's task queue, and a form in that frame with no `target` submits the
    // FRAME (measured).
    let initiator = context_of_element(scope, id);
    let target_name = submitter_or_form(scope, form, submitter, "formtarget", "target").unwrap_or_default();
    let chosen = choose_target(scope, initiator, &target_name);
    navigate_target(scope, initiator, chosen, nav);
}

/// HTML's two per-form submission FLAGS, and the reason they are not optional.
///
/// "Firing submission events" and "constructing the entry list" both exist to stop the algorithm
/// re-entering itself, and a form submission that really navigates re-enters constantly:
/// `form.requestSubmit()` from inside a `submit` listener fires another `submit` event, whose
/// listener calls `requestSubmit()` again. Without the flags that is unbounded recursion -- it does
/// not fail an assertion, it takes V8 down with `Check failed: IsOnCentralStack()`, which is what
/// `html/semantics/forms/form-submission-0/form-double-submit-requestsubmit.html` did to a WPT shard
/// the first time this algorithm navigated for real.
///
/// `submit()` is NOT stopped by either: the spec checks "firing submission events" only on the path
/// that FIRES them, and the same WPT file asserts a `form.submit()` from inside a `submit` listener
/// supersedes the submission that is running.
#[derive(Default)]
struct FormFlags {
    firing: std::collections::HashSet<u32>,
    constructing: std::collections::HashSet<u32>,
}

fn with_flags<R>(scope: &mut v8::PinScope, f: impl FnOnce(&mut FormFlags) -> R) -> R {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<FormFlags>().is_none() {
        st.put(FormFlags::default());
    }
    f(st.borrow_mut::<FormFlags>())
}

/// HTML's "can navigate" for a form: connected to a document, and that document is the one its
/// browsing context is showing.
fn can_navigate(scope: &mut v8::PinScope, id: u32) -> bool {
    element_document_is_current(scope, id)
}

/// "Statically validate the constraints", through every control's OWN `checkValidity` so the `invalid`
/// events fire where the spec puts them -- the same walk `cb_form_check_validity` runs, which is why
/// it is one function and not two.
fn static_validation(scope: &mut v8::PinScope, id: u32) -> bool {
    let mut ok = true;
    for c in form_controls(scope, id) {
        if control_check_validity(scope, c) == Some(false) {
            ok = false;
        }
    }
    ok
}

/// `method=dialog`: close the nearest ancestor `<dialog>` with the submitter's value.
fn close_ancestor_dialog(scope: &mut v8::PinScope, form: v8::Local<v8::Object>, submitter: Option<v8::Local<v8::Object>>) {
    let sel = str_val(scope, "dialog");
    let Some(d) = pristine_call(scope, form.into(), "Element.closest", &[sel]).filter(|v| v.is_object()) else { return };
    let rv: v8::Local<v8::Value> = match submitter.and_then(|s| attr(scope, s, "value")) {
        Some(v) => str_val(scope, &v),
        None => str_val(scope, ""),
    };
    pristine_call(scope, d, "HTMLDialogElement.close", &[rv]);
}

/// `submit()` -- the form-submission algorithm with "submitted from `submit()` method" set.
/// `reset()`: fire a cancelable, bubbling `reset` at the form, and unless a listener cancelled it,
/// reset every control the form owns (`korvus_dom::Dom::reset_form`). It was a D2 stub: a page's
/// "clear" button left every field as the user had filled it.
fn cb_form_reset<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(id) = this_form(scope, &args) else { return };
    let form = args.this();
    let Some(ev) = mint_event(scope, "Event", "reset", true, true) else { return };
    if !et_dispatch(scope, form.into(), ev) {
        return;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    op_state.borrow_mut().borrow_mut::<crate::binding::HostDom>().0.reset_form(id);
}

fn cb_form_submit<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    if this_form(scope, &args).is_none() {
        return;
    }
    submit_form(scope, args.this(), None, true);
}

/// `requestSubmit(submitter?)` -- the same algorithm, with the submitter checks in front.
///
/// The three checks and their exact messages are Chrome 151's, measured (they are quoted in this
/// file's header, which is where the follow-up this unit IS was asked to read them from). They run
/// BEFORE constraint validation.
fn cb_form_request_submit<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    if this_form(scope, &args).is_none() {
        return;
    }
    let form = args.this();
    let arg = args.get(0);
    let submitter = if arg.is_null_or_undefined() {
        None
    } else {
        let Ok(o) = v8::Local::<v8::Object>::try_from(arg) else {
            throw_type_error(
                scope,
                "Failed to execute 'requestSubmit' on 'HTMLFormElement': parameter 1 is not of type 'HTMLElement'.",
            );
            return;
        };
        if el_id_of(scope, o).is_none() {
            throw_type_error(
                scope,
                "Failed to execute 'requestSubmit' on 'HTMLFormElement': parameter 1 is not of type 'HTMLElement'.",
            );
            return;
        }
        if !is_submit_button(scope, o) {
            throw_type_error(
                scope,
                "Failed to execute 'requestSubmit' on 'HTMLFormElement': The specified element is not a submit button.",
            );
            return;
        }
        let formv: v8::Local<v8::Value> = form.into();
        let owner = control_form(scope, o);
        if owner.map(|v| v != formv).unwrap_or(true) {
            throw_dom(
                scope,
                "Failed to execute 'requestSubmit' on 'HTMLFormElement': The specified element is not owned by this form element.",
                "NotFoundError",
            );
            return;
        }
        Some(o)
    };
    submit_form(scope, form, submitter, false);
}

/// Is `el` a submit button -- a `<button>` whose `type` is `submit` (the default), or an
/// `<input type=submit|image>`?
pub(crate) fn is_submit_button(scope: &mut v8::PinScope, el: v8::Local<v8::Object>) -> bool {
    let tag = crate::page::tag_name_of(scope, (el).into()).unwrap_or_default();
    if tag.eq_ignore_ascii_case("button") {
        let ty = attr(scope, el, "type").unwrap_or_else(|| "submit".to_string()).to_ascii_lowercase();
        return ty == "submit";
    }
    if tag.eq_ignore_ascii_case("input") {
        let ty = attr(scope, el, "type").unwrap_or_default().to_ascii_lowercase();
        return ty == "submit" || ty == "image";
    }
    false
}

/// `HTMLFormElement::SubmitImplicitly` (`html_form_element.cc:539-577`): Enter in one of the
/// form's fields. The FIRST control in tree order that can be a submit button decides: an enabled one is
/// clicked -- a simulated, trusted `click` carrying the key's modifiers, whose activation behaviour
/// submits with it as the submitter; a disabled one stops everything when the field is a text field
/// (`from_trigger`, its `CanTriggerImplicitSubmission()`). With no submit button the form submits with no
/// submitter, but only when the field can trigger it and is the form's ONLY such field.
///
/// Recorded with real X11 keys on stock Chrome 150 (`tests/fixtures/key-events/`): Enter in a text field
/// of a form with a `<button>` fires `change`, then the button's `click` (a `PointerEvent`, `pointerId`
/// -1), then `submit` with the button as `submitter`; with one text field and no button, `submit` with no
/// submitter; with two text fields and no button, nothing. Returns the events dispatched by this step.
pub(crate) fn submit_implicitly<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    field: v8::Local<v8::Object>,
    from_trigger: bool,
    mods: crate::core::events::typing::KeyMods,
    timestamp_ms: f64,
) -> usize {
    let Some(form) = control_form(scope, field).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return 0 };
    let Some(id) = el_id_of(scope, form) else { return 0 };
    let mut triggers = 0;
    for c in form_controls(scope, id) {
        if is_submit_button(scope, c) {
            let sel = str_val(scope, ":disabled");
            let disabled = pristine_call(scope, c.into(), "Element.matches", &[sel]).is_some_and(|v| v.boolean_value(scope));
            if !disabled {
                crate::core::events::typing::dispatch_simulated_click(scope, c, mods, timestamp_ms);
                return 1;
            }
            if from_trigger {
                return 0;
            }
            continue;
        }
        // `HTMLInputElement::CanTriggerImplicitSubmission()` is `IsTextField()`.
        let tag = crate::page::tag_name_of(scope, c.into()).unwrap_or_default();
        if tag.eq_ignore_ascii_case("input") {
            let ty = attr(scope, c, "type").map(|t| t.trim().to_ascii_lowercase()).unwrap_or_default();
            if crate::core::events::typing::text_entry_type(&ty) {
                triggers += 1;
            }
        }
    }
    if from_trigger && triggers == 1 {
        submit_form(scope, form, None, false);
        return 1;
    }
    0
}

/// A submit button's ACTIVATION BEHAVIOUR: submit its form, with itself as the submitter. Run after a
/// `click` that nothing cancelled (`core/dom/element.rs`'s `click()`).
pub(crate) fn run_submit_activation<'s>(scope: &mut v8::PinScope<'s, '_>, el: v8::Local<'s, v8::Object>) {
    if !is_submit_button(scope, el) {
        return;
    }
    let Some(form) = control_form(scope, el).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
    else {
        return;
    };
    submit_form(scope, form, Some(el), false);
}

pub fn install_html_form_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::core::dom::element::cb_reflect_str_set;
    use crate::core::html::html_input_element::acc2;
    match member.name {
        "action" => acc2(scope, proto, member, cb_form_action_get, None, cb_reflect_str_set, Some("action")),
        "elements" => getter_row(scope, proto, member, cb_form_elements_get),
        "length" => getter_row(scope, proto, member, cb_form_length_get),
        "checkValidity" | "reportValidity" => method_row(scope, proto, member, cb_form_check_validity),
        "submit" => method_row(scope, proto, member, cb_form_submit),
        "reset" => method_row(scope, proto, member, cb_form_reset),
        "requestSubmit" => method_row(scope, proto, member, cb_form_request_submit),
        other => debug_assert!(false, "install_html_form_element: unexpected member {other}"),
    }
}

// `enctype` and `encoding` are two IDL names for ONE content attribute (`enctype`), which is why the
// two rows below carry the SAME `cfg` -- not a second piece of state. The `interface!` grammar takes a
// string literal for `cfg`, so the config is spelled twice; the fact that it must be the same string is
// pinned by `tests/forms_live.rs` (both members answer the same value and write the same attribute).
webapi::interface! {
    "HTMLFormElement" : "HTMLElement" [element, window] {
        ctor   "HTMLFormElement"                     len 0                   = stub,
        attr   "acceptCharset"    : DomString cfg "accept-charset"            = reflect_str,
        attr   "action"           : UsvString cfg "action"                    = install_html_form_element,
        attr   "autocomplete"     : DomString cfg "autocomplete|on,off|on|on" = reflect_enum,
        attr   "enctype"          : DomString cfg "enctype|application/x-www-form-urlencoded,multipart/form-data,text/plain|application/x-www-form-urlencoded|application/x-www-form-urlencoded" = reflect_enum,
        attr   "encoding"         : DomString cfg "enctype|application/x-www-form-urlencoded,multipart/form-data,text/plain|application/x-www-form-urlencoded|application/x-www-form-urlencoded" = reflect_enum,
        attr   "method"           : DomString cfg "method|get,post,dialog|get|get" = reflect_enum,
        attr   "name"             : DomString cfg "name"                      = reflect_str,
        attr   "noValidate"       : Boolean   cfg "novalidate"                = reflect_bool,
        attr   "target"           : DomString cfg "target"                    = reflect_str,
        attr   "rel"              : DomString cfg "rel"                       = reflect_str,
        attr   "relList"          : Object    cfg "rel"                       = reflect_tokens,
        attr   "elements"         : Object    readonly                        = install_html_form_element,
        attr   "length"           : Integer   readonly                        = install_html_form_element,
        method "checkValidity"    : Boolean   len 0                           = install_html_form_element,
        method "reportValidity"   : Boolean   len 0                           = install_html_form_element,
        method "requestSubmit"    : Undefined len 0                           = install_html_form_element,
        method "reset"            : Undefined len 0                           = install_html_form_element,
        method "submit"           : Undefined len 0                           = install_html_form_element,
        method "@@iterator" : Object len 0 hidden cfg "values" = array_proto_fn
    }
}

// ── named and indexed properties ──────────────────────────────────────────────────
//
// `form.q` is the control named or id'd "q" (a `RadioNodeList` when several are), `form[0]` the first
// listed control, and `form.length` their count -- Chrome 150, measured (`tools/realweb/seed/np`).
// korvus answered undefined, so crt.sh's `document.search.q` threw. HTMLFormElement is
// [LegacyOverrideBuiltIns]: a control named like a member wins, so the interceptor runs first and
// answers "not mine" at once when no listed control carries the name.

fn form_named_hit(scope: &mut v8::PinScope, form: u32, name: &str) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let dom = &st.borrow::<crate::binding::HostDom>().0;
    dom.descendants_matching(form, crate::core::dom::collections::LISTED_CONTROLS)
        .unwrap_or_default()
        .into_iter()
        .any(|c| dom.get_attribute(c, "name") == Some(name) || dom.get_attribute(c, "id") == Some(name))
}

fn form_controls_list<'s>(scope: &mut v8::PinScope<'s, '_>, form: u32) -> Option<v8::Local<'s, v8::Object>> {
    use crate::core::dom::collections::{make_live, Kind};
    let v = make_live(scope, "HTMLFormControlsCollection", form, Kind::FormControls, "", None);
    v8::Local::<v8::Object>::try_from(v).ok()
}

fn form_controls_len(scope: &mut v8::PinScope, form: u32) -> u32 {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let dom = &st.borrow::<crate::binding::HostDom>().0;
    dom.descendants_matching(form, crate::core::dom::collections::LISTED_CONTROLS).map(|v| v.len() as u32).unwrap_or(0)
}

fn cb_form_named_get<'s>(scope: &mut v8::PinScope<'s, '_>, key: v8::Local<'s, v8::Name>, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue) -> v8::Intercepted {
    if !key.is_string() { return v8::Intercepted::kNo; }
    let name = key.to_rust_string_lossy(scope);
    if name.is_empty() { return v8::Intercepted::kNo; }
    let Some(form) = crate::binding::protos::unwrap_id(scope, args.holder().into()) else { return v8::Intercepted::kNo };
    if !form_named_hit(scope, form, &name) { return v8::Intercepted::kNo; }
    let Some(list) = form_controls_list(scope, form) else { return v8::Intercepted::kNo };
    let n = crate::binding::protos::str_val(scope, &name);
    match pristine_call(scope, list.into(), "HTMLFormControlsCollection.namedItem", &[n]) {
        Some(v) if !v.is_null_or_undefined() => { rv.set(v); v8::Intercepted::kYes }
        _ => v8::Intercepted::kNo,
    }
}

fn cb_form_named_query<'s>(scope: &mut v8::PinScope<'s, '_>, key: v8::Local<'s, v8::Name>, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue<v8::Integer>) -> v8::Intercepted {
    if !key.is_string() { return v8::Intercepted::kNo; }
    let name = key.to_rust_string_lossy(scope);
    let Some(form) = crate::binding::protos::unwrap_id(scope, args.holder().into()) else { return v8::Intercepted::kNo };
    if name.is_empty() || !form_named_hit(scope, form, &name) { return v8::Intercepted::kNo; }
    // [LegacyUnenumerableNamedProperties]
    rv.set(v8::Integer::new(scope, v8::PropertyAttribute::DONT_ENUM.as_u32() as i32));
    v8::Intercepted::kYes
}

fn cb_form_idx_get<'s>(scope: &mut v8::PinScope<'s, '_>, index: u32, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue) -> v8::Intercepted {
    let Some(form) = crate::binding::protos::unwrap_id(scope, args.holder().into()) else { return v8::Intercepted::kNo };
    if index >= form_controls_len(scope, form) { return v8::Intercepted::kNo; }
    let Some(list) = form_controls_list(scope, form) else { return v8::Intercepted::kNo };
    match list.get_index(scope, index) {
        Some(v) if !v.is_null_or_undefined() => { rv.set(v); v8::Intercepted::kYes }
        _ => v8::Intercepted::kNo,
    }
}

fn cb_form_idx_query<'s>(scope: &mut v8::PinScope<'s, '_>, index: u32, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue<v8::Integer>) -> v8::Intercepted {
    let Some(form) = crate::binding::protos::unwrap_id(scope, args.holder().into()) else { return v8::Intercepted::kNo };
    if index >= form_controls_len(scope, form) { return v8::Intercepted::kNo; }
    rv.set(v8::Integer::new(scope, v8::PropertyAttribute::READ_ONLY.as_u32() as i32));
    v8::Intercepted::kYes
}

fn cb_form_idx_enum<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue<v8::Array>) {
    let n = crate::binding::protos::unwrap_id(scope, args.holder().into()).map(|f| form_controls_len(scope, f)).unwrap_or(0);
    let arr = v8::Array::new(scope, n as i32);
    for i in 0..n {
        let k = v8::Integer::new_from_unsigned(scope, i);
        arr.set_index(scope, i, k.into());
    }
    rv.set(arr);
}

pub(crate) fn form_named_config<'s>() -> v8::NamedPropertyHandlerConfiguration<'s> {
    use deno_core::v8::MapFnTo;
    v8::NamedPropertyHandlerConfiguration::new().getter_raw(crate::binding::refs::c(icpt!(named_get, "HTMLFormElement", cb_form_named_get).map_fn_to())).query_raw(crate::binding::refs::c(icpt!(named_query, "HTMLFormElement", cb_form_named_query).map_fn_to()))
}

pub(crate) fn form_indexed_config<'s>() -> v8::IndexedPropertyHandlerConfiguration<'s> {
    use deno_core::v8::MapFnTo;
    v8::IndexedPropertyHandlerConfiguration::new()
        .getter_raw(crate::binding::refs::c(icpt!(idx_get, "HTMLFormElement", cb_form_idx_get).map_fn_to()))
        .query_raw(crate::binding::refs::c(icpt!(idx_query, "HTMLFormElement", cb_form_idx_query).map_fn_to()))
        .enumerator_raw(crate::binding::refs::c(icpt!(idx_keys, "HTMLFormElement", cb_form_idx_enum).map_fn_to()))
}
