//! ACTIVATION BEHAVIOUR: what a `click` does besides its listeners, run by the dispatch itself
//! (DOM "dispatch", the activation target and its legacy-pre-activation / legacy-canceled-activation / activation
//! behaviour steps).
//!
//! Only a submit button's activation existed, and only through `el.click()`: a link followed nothing, a checkbox
//! or radio did not change, a `<label>` did not reach its control, a `<summary>` did not open its `<details>` --
//! for `el.click()`, for `dispatchEvent(new MouseEvent("click"))` and for a trusted click alike. Measured on stock
//! Chrome 150 (2026-09-30, `tools/realweb/seed/activation`):
//!
//! * a checkbox is toggled BEFORE the click listeners run (they read the new state), then `input` and `change`
//!   fire; a cancelled click puts the old state back and fires neither; a radio likewise, with the one that was
//!   checked restored on cancel;
//! * a `<label>` sends the click on to its control; a `<summary>` toggles its `<details>`;
//! * a link to the document's own URL with a fragment is a same-document navigation (`location.hash`,
//!   `hashchange`); a `javascript:` link runs; a `download` link does not navigate; any other link navigates the
//!   context its `target` names -- `_top` from a frame navigates the page (unless a sandbox forbids it, F35).
//!
//! The activation target is the event target when it has an activation behaviour, else (for a bubbling click)
//! the nearest ancestor that has one: a click on a `<span>` inside an `<a>` follows the link.
//!
//! Every `<input>` and `<button>` HAS one, whatever its type (HTML "the input element's activation behavior",
//! "the button element's activation behavior"): the legacy-pre-activation step reads the type the click
//! starts with, the activation step the type the element has once the listeners are done -- a click
//! listener that turns a text field into a submit button submits the form (WPT `input-type-change-submit`,
//! `form-double-submit-input-type-change`: Chrome 150 20/20 each). The legacy-canceled-activation step reads
//! the type NOW too: a checkbox or radio a listener turned into a date field keeps what the click did to it
//! (Chrome 150 fails WPT `Event-dispatch-click.tentative`'s two "morphed" cases, measured 2026-09-30).
//!
//! Every read and write here is the realm's OWN member (`protos::pristine_call` / `pristine_get` /
//! `pristine_set`), never the page's: Blink's activation steps read and set the element's state in C++, and
//! these went through `checked`, `type`, `form`, `control`, `matches`, `getRootNode`, `click` as a page would,
//! so a page that wrapped any of them saw the click's own steps as calls of its
//! (`tests/engine_reads_live.rs`: Chrome 150 calls none).

use deno_core::v8;

use crate::binding::protos::{pristine_call, pristine_get, pristine_set, str_val};

/// An element with an activation behaviour, and what kind.
pub(crate) enum Target<'s> {
    Link(v8::Local<'s, v8::Object>),
    Input(v8::Local<'s, v8::Object>),
    Button(v8::Local<'s, v8::Object>),
    Label(v8::Local<'s, v8::Object>),
    Summary(v8::Local<'s, v8::Object>),
}

/// The state a legacy-pre-activation behaviour changed, for the cancel step.
pub(crate) enum Before<'s> {
    None,
    Checkbox { checked: bool, indeterminate: bool },
    Radio { checked: bool, previous: Option<v8::Local<'s, v8::Object>> },
}

fn attr(scope: &mut v8::PinScope, el: v8::Local<v8::Object>, name: &str) -> Option<String> {
    let n = str_val(scope, name);
    pristine_call(scope, el.into(), "Element.getAttribute", &[n]).filter(|v| !v.is_null()).map(|v| v.to_rust_string_lossy(scope))
}

fn local_name(scope: &mut v8::PinScope, el: v8::Local<v8::Object>) -> Option<String> {
    let id = crate::binding::protos::unwrap_id(scope, el.into())?;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let dom = &st.try_borrow::<crate::binding::host_dom::HostDom>()?.0;
    if !dom.is_html_ns(id) {
        return None;
    }
    dom.tag_name(id).map(|t| t.to_ascii_lowercase())
}

/// A boolean member read with the realm's own getter (`"HTMLInputElement.checked"`).
fn bool_prop(scope: &mut v8::PinScope, el: v8::Local<v8::Object>, member: &str) -> bool {
    pristine_get(scope, el.into(), member).is_some_and(|v| v.boolean_value(scope))
}

fn set_bool(scope: &mut v8::PinScope, el: v8::Local<v8::Object>, member: &str, b: bool) {
    let v = v8::Boolean::new(scope, b);
    pristine_set(scope, el.into(), member, v.into());
}

/// The interface whose `type` / `form` an `<input>` or `<button>` answers.
fn control_iface(scope: &mut v8::PinScope, el: v8::Local<v8::Object>) -> &'static str {
    if local_name(scope, el).as_deref() == Some("button") { "HTMLButtonElement" } else { "HTMLInputElement" }
}

/// The control's type now, lowercased (`input` defaults to `text`, `button` to `submit`).
fn control_type(scope: &mut v8::PinScope, el: v8::Local<v8::Object>) -> String {
    let member = format!("{}.type", control_iface(scope, el));
    pristine_get(scope, el.into(), &member).map(|v| v.to_rust_string_lossy(scope).to_ascii_lowercase()).unwrap_or_default()
}

/// The control's form owner, if any.
fn form_of<'s>(scope: &mut v8::PinScope<'s, '_>, el: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::Value>> {
    let member = format!("{}.form", control_iface(scope, el));
    pristine_get(scope, el.into(), &member).filter(|f| f.is_object())
}

/// `:disabled` -- its own attribute or a disabled fieldset's.
fn disabled(scope: &mut v8::PinScope, el: v8::Local<v8::Object>) -> bool {
    let sel = str_val(scope, ":disabled");
    pristine_call(scope, el.into(), "Element.matches", &[sel]).is_some_and(|v| v.boolean_value(scope))
}

/// What `el` does when activated, if anything.
fn kind<'s>(scope: &mut v8::PinScope<'s, '_>, el: v8::Local<'s, v8::Object>) -> Option<Target<'s>> {
    let tag = local_name(scope, el)?;
    Some(match tag.as_str() {
        "a" | "area" if attr(scope, el, "href").is_some() => Target::Link(el),
        "input" => Target::Input(el),
        "button" => Target::Button(el),
        "label" => Target::Label(el),
        "summary" => Target::Summary(el),
        _ => return None,
    })
}

/// The activation target of a `click` dispatched at `target`: itself, or for a bubbling click the nearest
/// ancestor with an activation behaviour.
pub(crate) fn find<'s>(scope: &mut v8::PinScope<'s, '_>, target: v8::Local<'s, v8::Value>, bubbles: bool) -> Option<Target<'s>> {
    let mut cur = v8::Local::<v8::Object>::try_from(target).ok()?;
    for depth in 0..256 {
        if let Some(t) = kind(scope, cur) {
            return Some(t);
        }
        if !bubbles && depth == 0 {
            return None;
        }
        let p = pristine_get(scope, cur.into(), "Node.parentNode")?;
        cur = v8::Local::<v8::Object>::try_from(p).ok()?;
    }
    None
}

/// Legacy-pre-activation behaviour: a checkbox toggles and a radio checks itself, BEFORE the listeners run.
pub(crate) fn pre<'s>(scope: &mut v8::PinScope<'s, '_>, t: &Target<'s>) -> Before<'s> {
    // A DISABLED checkbox or radio still has its activation behaviour (HTML: only a control that is not a
    // checkbox or radio stops at "not mutable"; WPT `Event-dispatch-click`'s disabled cases, Chrome 33/33).
    let Target::Input(el) = t else { return Before::None };
    match control_type(scope, *el).as_str() {
        "checkbox" => {
            let checked = bool_prop(scope, *el, "HTMLInputElement.checked");
            let indeterminate = bool_prop(scope, *el, "HTMLInputElement.indeterminate");
            set_bool(scope, *el, "HTMLInputElement.checked", !checked);
            set_bool(scope, *el, "HTMLInputElement.indeterminate", false);
            Before::Checkbox { checked, indeterminate }
        }
        "radio" => {
            let checked = bool_prop(scope, *el, "HTMLInputElement.checked");
            let previous = if checked { None } else { checked_in_group(scope, *el) };
            set_bool(scope, *el, "HTMLInputElement.checked", true);
            Before::Radio { checked, previous }
        }
        _ => Before::None,
    }
}

/// The radio of `el`'s group (same name, same form owner, same tree) that is checked now.
fn checked_in_group<'s>(scope: &mut v8::PinScope<'s, '_>, el: v8::Local<'s, v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    let name = attr(scope, el, "name").filter(|n| !n.is_empty())?;
    let form = form_of(scope, el);
    let root = pristine_call(scope, el.into(), "Node.getRootNode", &[])?;
    let sel = str_val(scope, "input[type=radio i]");
    let list = crate::binding::protos::pristine_query_all(scope, root, sel)?;
    let n = pristine_get(scope, list.into(), "NodeList.length").and_then(|v| v.uint32_value(scope)).unwrap_or(0);
    for i in 0..n {
        let Some(r) = list.get_index(scope, i).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { continue };
        if r == el || attr(scope, r, "name").as_deref() != Some(name.as_str()) {
            continue;
        }
        let rf = form_of(scope, r);
        let same_form = match (form, rf) {
            (Some(a), Some(b)) => a.strict_equals(b),
            (None, None) => true,
            _ => false,
        };
        if same_form && bool_prop(scope, r, "HTMLInputElement.checked") {
            return Some(r);
        }
    }
    None
}

fn fire<'s>(scope: &mut v8::PinScope<'s, '_>, el: v8::Local<'s, v8::Object>, ty: &str, composed: bool) {
    let Some(ctor) = crate::binding::protos::iface_object(scope, "Event").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return };
    // the init dictionary has no prototype and only own data members: its conversion reads nothing a page put on
    // `Object.prototype`
    let init = crate::binding::protos::init_dict(scope);
    let null: v8::Local<v8::Value> = v8::null(scope).into();
    init.set_prototype(scope, null);
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    let k = v8::String::new(scope, "bubbles").unwrap();
    init.create_data_property(scope, k.into(), t);
    if composed {
        let k = v8::String::new(scope, "composed").unwrap();
        init.create_data_property(scope, k.into(), t);
    }
    let tyv = str_val(scope, ty);
    let Some(ev) = ctor.new_instance(scope, &[tyv, init.into()]) else { return };
    crate::core::html::html_dialog_element::mark_trusted(scope, ev);
    crate::core::events::event_target::et_dispatch(scope, el.into(), ev);
}

/// Legacy-canceled-activation behaviour: the click was cancelled, so what `pre` changed goes back -- while the
/// element still has the type it had (see the module docs).
pub(crate) fn cancel<'s>(scope: &mut v8::PinScope<'s, '_>, t: &Target<'s>, before: Before<'s>) {
    let now = match t {
        Target::Input(el) => control_type(scope, *el),
        _ => return,
    };
    match (t, before) {
        (Target::Input(el), Before::Checkbox { checked, indeterminate }) if now == "checkbox" => {
            set_bool(scope, *el, "HTMLInputElement.checked", checked);
            set_bool(scope, *el, "HTMLInputElement.indeterminate", indeterminate);
        }
        (Target::Input(el), Before::Radio { checked, previous }) if now == "radio" => {
            if !checked {
                match previous {
                    Some(p) => set_bool(scope, p, "HTMLInputElement.checked", true),
                    None => set_bool(scope, *el, "HTMLInputElement.checked", false),
                }
            }
        }
        _ => {}
    }
}

/// The activation behaviour, for a click nothing cancelled. `target` is the event's own target.
pub(crate) fn run<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    t: &Target<'s>,
    before: Before<'s>,
    target: v8::Local<'s, v8::Value>,
    trusted: bool,
    ev: v8::Local<'s, v8::Object>,
) {
    match t {
        // The type the element has NOW decides (see the module docs).
        Target::Input(el) => {
            let ty = control_type(scope, *el);
            if !matches!(ty.as_str(), "checkbox" | "radio") && disabled(scope, *el) {
                return;
            }
            match ty.as_str() {
                "checkbox" => {
                    if bool_prop(scope, *el, "Node.isConnected") {
                        fire(scope, *el, "input", true);
                        fire(scope, *el, "change", false);
                    }
                }
                "radio" => {
                    let was = matches!(before, Before::Radio { checked: true, .. });
                    if !was && bool_prop(scope, *el, "Node.isConnected") {
                        fire(scope, *el, "input", true);
                        fire(scope, *el, "change", false);
                    }
                }
                "submit" | "image" => crate::core::html::html_form_element::run_submit_activation(scope, *el),
                "reset" => reset_form(scope, *el),
                "file" => open_file_chooser(scope, *el),
                _ => {}
            }
        }
        Target::Button(el) => {
            if disabled(scope, *el) {
                return;
            }
            match control_type(scope, *el).as_str() {
                "reset" => reset_form(scope, *el),
                "button" => {}
                _ => crate::core::html::html_form_element::run_submit_activation(scope, *el),
            }
        }
        Target::Label(el) => {
            // The click goes on to the labelled control -- unless it came from inside that control already.
            let Some(control) = pristine_get(scope, (*el).into(), "HTMLLabelElement.control").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
            let inside = pristine_call(scope, control.into(), "Node.contains", &[target]).is_some_and(|v| v.boolean_value(scope));
            if !inside {
                label_click(scope, control, ev);
            }
        }
        Target::Summary(el) => {
            // Only a `<details>`'s FIRST `<summary>` child is its summary.
            let Some(details) = pristine_get(scope, (*el).into(), "Node.parentNode").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
            if local_name(scope, details).as_deref() != Some("details") {
                return;
            }
            let sel = str_val(scope, ":scope > summary");
            let first = pristine_call(scope, details.into(), "Element.querySelector", &[sel]);
            if !first.is_some_and(|f| f.strict_equals((*el).into())) {
                return;
            }
            let open = bool_prop(scope, details, "HTMLDetailsElement.open");
            set_bool(scope, details, "HTMLDetailsElement.open", !open);
        }
        Target::Link(el) => crate::core::html::browsing_context::follow_hyperlink(scope, *el, trusted),
    }
}

/// Blink's `HTMLLabelElement::DefaultEventHandler` for a click on a label (FIXER C6): a MOUSE-focusable control
/// takes the focus, then gets a click made from this one (`EventDispatcher::DispatchSimulatedClick`, the
/// user-agent scope of `SimulatedEventUtil`): a `PointerEvent`, bubbling, cancelable, composed, with the label
/// click's pointer id, type, primariness, `detail`, client and screen coordinates and modifiers, trusted when it
/// was. Measured on stock Chrome 150 with a real click on a `<label for>`: `focus`/`focusin` at the control, then
/// its `click` -- `isTrusted` true, `detail` 1, the label click's `clientX/Y`, `offsetX/Y` from the control -- then
/// its `input` and `change`. It went through `HTMLElement.click`, an untrusted script click with no position,
/// and moved no focus.
fn label_click<'s>(scope: &mut v8::PinScope<'s, '_>, control: v8::Local<'s, v8::Object>, underlying: v8::Local<'s, v8::Object>) {
    use crate::core::events::event::{evt_slot_read, evt_slot_write_num};
    let Some(id) = crate::binding::protos::unwrap_id(scope, control.into()) else { return };
    if disabled_control(scope, control) {
        return;
    }
    let focusable = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<crate::binding::host_dom::HostDom>().is_some_and(|h| crate::core::events::focus::focusable(&h.0, id))
    };
    if focusable {
        let now = crate::core::events::focus::page_now(scope);
        crate::core::events::focus::focus_element(scope, id, now);
    }
    let is_mouse = crate::core::events::ctor::has_brand(scope, underlying.into(), "MouseEvent");
    let is_pointer = crate::core::events::ctor::has_brand(scope, underlying.into(), "PointerEvent");
    let tk = crate::core::events::event::trusted_key(scope);
    let trusted = underlying.get_private(scope, tk).is_some_and(|v| v.is_true());
    let realm = crate::core::html::browsing_context::realm_of_node(scope, id).unwrap_or_else(|| scope.get_current_context());
    let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
    let Some(ctor) = crate::binding::protos::iface_object(scope, "PointerEvent").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else { return };
    let init = crate::binding::protos::init_dict(scope);
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    for k in ["bubbles", "cancelable", "composed"] {
        crate::binding::protos::set_prop(scope, init, k, t);
    }
    let view: v8::Local<v8::Value> = scope.get_current_context().global(scope).into();
    crate::binding::protos::set_prop(scope, init, "view", view);
    for k in ["ctrlKey", "shiftKey", "altKey", "metaKey"] {
        if let Some(v) = evt_slot_read(scope, underlying, k) {
            crate::binding::protos::set_prop(scope, init, k, v);
        }
    }
    if is_mouse {
        for k in ["screenX", "screenY", "clientX", "clientY"] {
            copy_number(scope, underlying, init, k);
        }
    }
    if is_pointer {
        for k in ["pointerId", "detail"] {
            copy_number(scope, underlying, init, k);
        }
        if let Some(v) = evt_slot_read(scope, underlying, "pointerType") {
            crate::binding::protos::set_prop(scope, init, "pointerType", v);
        }
        if let Some(v) = evt_slot_read(scope, underlying, "isPrimary") {
            crate::binding::protos::set_prop(scope, init, "isPrimary", v);
        }
    } else {
        let v: v8::Local<v8::Value> = v8::Integer::new(scope, -1).into();
        crate::binding::protos::set_prop(scope, init, "pointerId", v);
    }
    let ty = str_val(scope, "click");
    let Some(click) = ctor.new_instance(scope, &[ty, init.into()]) else { return };
    if trusted {
        crate::core::html::html_dialog_element::mark_trusted(scope, click);
    }
    let ts = evt_slot_read(scope, underlying, "timeStamp").and_then(|v| v.number_value(scope));
    if let Some(ts) = ts {
        evt_slot_write_num(scope, click, "timeStamp", ts);
    }
    if is_mouse {
        crate::core::events::pointer_input::stamp_relative_position(scope, click, id);
    }
    crate::core::events::event_target::et_dispatch(scope, control.into(), click);
}

/// Copy the number in `from`'s slot `k` into the init dictionary `init` (0 when unset).
fn copy_number(scope: &mut v8::PinScope, from: v8::Local<v8::Object>, init: v8::Local<v8::Object>, k: &str) {
    let n = crate::core::events::event::evt_slot_read(scope, from, k).and_then(|v| v.number_value(scope)).unwrap_or(0.0);
    let v: v8::Local<v8::Value> = v8::Number::new(scope, n).into();
    crate::binding::protos::set_prop(scope, init, k, v);
}

/// A form control that is `:disabled` gets no simulated click (`EventDispatcher::DispatchSimulatedClick`).
fn disabled_control(scope: &mut v8::PinScope, el: v8::Local<v8::Object>) -> bool {
    matches!(local_name(scope, el).as_deref(), Some("button" | "input" | "select" | "textarea" | "fieldset")) && disabled(scope, el)
}

fn reset_form(scope: &mut v8::PinScope, el: v8::Local<v8::Object>) {
    if let Some(form) = form_of(scope, el) {
        pristine_call(scope, form, "HTMLFormElement.reset", &[]);
    }
}

/// 7.3 (ABI 3 step 10): a click on an `<input type=file>` opens a file chooser. korvus has no OS chooser of
/// its own, so with no plugin loaded the control is left untouched -- its `files` stays the empty selection it
/// is today (invariant I9/J2), and the click just picks nothing. A loaded plugin may SUPPLY the selection
/// through `on_file_chooser`: `Some(paths)` sets the control's `files` to one File per absolute path (its bytes
/// read from disk, its name the path's final component) and fires `input` then `change`; `None` -- no plugin,
/// or a plugin that cancelled -- leaves the selection empty, exactly as a user who closed the chooser without
/// choosing. Mirrors how `window.rs` lets `on_dialog` answer confirm/prompt.
fn open_file_chooser<'s>(scope: &mut v8::PinScope<'s, '_>, el: v8::Local<'s, v8::Object>) {
    // A DevTools client that intercepts choosers (`Page.setInterceptFileChooserDialog`) is told, and answers by
    // setting the control's files itself.
    if crate::devtools_log::intercepting_file_chooser() {
        if let Some(id) = crate::binding::protos::unwrap_id(scope, el.into()) {
            let multiple = attr(scope, el, "multiple").is_some();
            crate::devtools_log::file_chooser_opened(id, multiple);
        }
        return;
    }
    if !plugin_host::any() {
        return;
    }
    let accept = attr(scope, el, "accept").unwrap_or_default();
    let Some(mut paths) = plugin_host::on_file_chooser(&accept) else { return };
    // A chooser on an input without `multiple` yields at most one file.
    if attr(scope, el, "multiple").is_none() {
        paths.truncate(1);
    }
    let mut files: Vec<v8::Global<v8::Value>> = Vec::with_capacity(paths.len());
    for path in &paths {
        let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.clone());
        let bytes = std::fs::read(path).unwrap_or_default();
        let lm = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as f64)
            .unwrap_or(0.0);
        // TODO(1021-verify): the File's MIME type is left "" -- there is no OS extension->type map here.
        let Some(f) = crate::core::fileapi::file::mint_file(scope, bytes, "", &name, lm) else { continue };
        let fv: v8::Local<v8::Value> = f.into();
        files.push(v8::Global::new(scope, fv));
    }
    if files.is_empty() {
        return;
    }
    let fl = crate::core::fileapi::file_list::mint_file_list(scope, files);
    // Set through the realm's own `files` setter, as the rest of this file sets `checked`: it stores the list
    // on the element and updates its file count, exactly as `input.files = list` from script does.
    let flv: v8::Local<v8::Value> = fl.into();
    pristine_set(scope, el.into(), "HTMLInputElement.files", flv);
    // A file selection fires `input` then `change` (as the checkbox toggle above does).
    // TODO(1021-verify): the task named `change`; a real `<input type=file>` selection fires `input` first too.
    if bool_prop(scope, el, "Node.isConnected") {
        fire(scope, el, "input", true);
        fire(scope, el, "change", false);
    }
}
