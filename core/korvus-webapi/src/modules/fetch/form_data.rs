//! `modules/fetch/form_data.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::{v8, CppgcBase};
use macros::webapi;
use crate::binding::HostDom;
use crate::core::dom::webidl_iter::{for_each_live_pairs, live_pair_iter};
use crate::core::dom::JsElement;

#[repr(C)]
#[derive(CppgcBase)]
pub struct FormData {
    d: std::cell::RefCell<Vec<(String, String)>>,
}

pub(crate) trait FormDataBehavior {
   fn construct(_scope: &mut v8::PinScope, _form: v8::Local<v8::Value>) -> FormData where Self: Sized { stub!("FormData::constructor") }
   fn append(&self, _k: String, _v: String)  { stub!("FormData::append") }
   fn set(&self, _k: String, _v: String)  { stub!("FormData::set") }
   fn get(&self, _k: String) -> Option<String> { stub!("FormData::get") }
   fn get_all(&self, _k: String) -> Vec<String> { stub!("FormData::getAll") }
   fn has(&self, _k: String) -> bool { stub!("FormData::has") }
   fn delete_(&self, _k: String)  { stub!("FormData::delete") }
}

#[webapi(base, own)]
impl FormData {
    #[constructor]
    #[cppgc]
    fn constructor(scope: &mut v8::PinScope, form: v8::Local<v8::Value>) -> Result<FormData, ::deno_error::JsErrorBox> { dispatch!("FormData::constructor", <FormData as FormDataBehavior>::construct(scope, form)) }
    // Declaration order drives the op2 prototype install order: keep it append, delete, get,
    // getAll, has, set so the live enumeration order matches the Brave-151 baseline (the JS glue
    // in dom.js appends entries/forEach/keys/values AFTER these six).
    #[fast]
    fn append(&self, #[string] k: String, #[string] v: String) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("FormData::append", <FormData as FormDataBehavior>::append(self, k, v)) }
    #[fast]
    #[rename("delete")]
    fn delete_(&self, #[string] k: String) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("FormData::delete", <FormData as FormDataBehavior>::delete_(self, k)) }
    #[string]
    fn get(&self, #[string] k: String) -> Result<Option<String>, ::deno_error::JsErrorBox> { dispatch!("FormData::get", <FormData as FormDataBehavior>::get(self, k)) }
    #[serde]
    fn get_all(&self, #[string] k: String) -> Result<Vec<String>, ::deno_error::JsErrorBox> { dispatch!("FormData::getAll", <FormData as FormDataBehavior>::get_all(self, k)) }
    #[fast]
    fn has(&self, #[string] k: String) -> Result<bool, ::deno_error::JsErrorBox> { dispatch!("FormData::has", <FormData as FormDataBehavior>::has(self, k)) }
    #[fast]
    fn set(&self, #[string] k: String, #[string] v: String) -> Result<(), ::deno_error::JsErrorBox> { dispatch!("FormData::set", <FormData as FormDataBehavior>::set(self, k, v)) }
}

impl FormDataBehavior for FormData {
    fn construct(scope: &mut v8::PinScope, form: v8::Local<v8::Value>) -> FormData { let fd = FormData { d: std::cell::RefCell::new(Vec::new()) };
        if let Some(form_el) = deno_core::cppgc::try_unwrap_cppgc_object::<JsElement>(scope, form) {
            let form_id = unsafe { form_el.as_ref() }.id;
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            let dom = &st.borrow::<HostDom>().0;
            // Fixed, always-valid selector: a parse error is impossible, so an empty fallback is fine.
            for cid in dom.descendants_matching(form_id, "input,select,textarea").unwrap_or_default() {
                let name = match dom.get_attribute(cid, "name") { Some(n) if !n.is_empty() => n.to_string(), _ => continue };
                if dom.get_attribute(cid, "disabled").is_some() { continue; }
                let ty = dom.get_attribute(cid, "type").unwrap_or("").to_lowercase();
                // An unchecked checkbox/radio submits nothing. What decides that is the element's
                // CHECKEDNESS, not its `checked` content attribute: `box.checked = true` from script
                // sets the former and deliberately leaves the latter alone (see html_input_element.rs),
                // so reading the attribute here would drop exactly the fields a script just ticked.
                if (ty == "checkbox" || ty == "radio") && !dom.checkedness(cid) { continue; }
                if ty == "submit" || ty == "button" || ty == "file" || ty == "reset" { continue; }
                let value = dom.control_value(cid);
                fd.d.borrow_mut().push((name, value));
            }
        }
        fd }
    fn append(&self, k: String, v: String)  { self.d.borrow_mut().push((k, v)); }
    fn set(&self, k: String, v: String)  { let mut b = self.d.borrow_mut();
        // Spec (webidl FormData.set): replace the FIRST entry named k in place and drop the rest;
        // append only when no entry with that name exists yet (position-preserving, unlike append).
        match b.iter().position(|x| x.0 == k) {
            Some(i) => {
                b[i].1 = v;
                let mut j = i + 1;
                while j < b.len() {
                    if b[j].0 == k { b.remove(j); } else { j += 1; }
                }
            }
            None => b.push((k, v)),
        } }
    fn get(&self, k: String) -> Option<String> { self.d.borrow().iter().find(|x| x.0 == k).map(|x| x.1.clone()) }
    fn get_all(&self, k: String) -> Vec<String> { self.d.borrow().iter().filter(|x| x.0 == k).map(|x| x.1.clone()).collect() }
    fn has(&self, k: String) -> bool { self.d.borrow().iter().any(|x| x.0 == k) }
    fn delete_(&self, k: String)  { self.d.borrow_mut().retain(|x| x.0 != k); }
}

pub(crate) fn fd_pairs(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> Option<Vec<(String, String)>> {
    let fd = deno_core::cppgc::try_unwrap_cppgc_object::<FormData>(scope, this)?;
    let pairs = unsafe { fd.as_ref() }.d.borrow().clone();
    Some(pairs)
}
fn cb_fd_foreach<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    for_each_live_pairs(scope, &args, "FormData");
}

fn cb_fd_keys<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let it = live_pair_iter(scope, "FormData", args.this().into(), 0);
    rv.set(it);
}

fn cb_fd_values<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let it = live_pair_iter(scope, "FormData", args.this().into(), 1);
    rv.set(it);
}

fn cb_fd_entries<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let it = live_pair_iter(scope, "FormData", args.this().into(), 2);
    rv.set(it);
}


/// The four rows of [`FORMDATA`] the class template cannot realise, plus the `@@iterator` alias.
/// The same shape as `headers::install_headers` and for the same reason: an `#[op2] impl` method
/// gets no scope, and these four need one.
pub fn install_form_data(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static crate::install::MemberDef) {
    use crate::install::live::rows::method_row;
    match member.name {
        "entries" => method_row(scope, proto, member, cb_fd_entries),
        "forEach" => method_row(scope, proto, member, cb_fd_foreach),
        "keys" => method_row(scope, proto, member, cb_fd_keys),
        "values" => method_row(scope, proto, member, cb_fd_values),
        // `FormData.prototype[Symbol.iterator] === FormData.prototype.entries` (Chrome 151, measured).
        "@@iterator" => {
            let k: v8::Local<v8::Value> = crate::binding::protos::member_key(scope, "entries").into();
            if let Some(f) = proto.get(scope, k) {
                crate::install::kits::define_value(scope, proto, member, f, true);
            }
        }
        other => debug_assert!(false, "install_form_data: unexpected member {other}"),
    }
}

// ── One-table definition ─────────────────────────────────────────────────────────
//
// Rows VERBATIM from the `FormData` block of `install/generated_registry.rs`, plus the `@@iterator`
// row. Six `cppgc` rows the class template realises (each verified and finished from its own
// `required` / `ty`), four `Install` rows that need a scope. `[value, both]` for the same reason
// `Headers` is: one table, two realms, no second shell set.
webapi::interface! {
    "FormData" [value, both, class FormData] {
        ctor          "FormData"                          len 0 = stub,
        method        "append"     : Undefined len 2 required 2  = cppgc,
        method        "delete"     : Undefined len 1 required 1  = cppgc,
        method        "get"        : Any       len 1 required 1 nullable = cppgc,
        method        "getAll"     : Sequence  len 1 required 1  = cppgc,
        method        "has"        : Boolean   len 1 required 1  = cppgc,
        method        "set"        : Undefined len 2 required 2  = cppgc,
        method        "entries"    : Any       len 0             = install_form_data,
        method        "forEach"    : Any       len 1             = install_form_data,
        method        "keys"       : Any       len 0             = install_form_data,
        method        "values"     : Any       len 0             = install_form_data,
        method        "@@iterator" : Any       len 0 hidden      = install_form_data
    }
}
