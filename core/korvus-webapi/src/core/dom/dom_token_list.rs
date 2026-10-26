//! `core/dom/dom_token_list.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use korvus_dom::Dom;
use deno_core::v8;
use crate::binding::HostDom;
use crate::core::dom::shared::{set_attribute_notify};

use crate::binding::protos::{str_val, throw_dom};

/// The two privates a token list carries ([`DTL_ID`], [`DTL_ATTR`]), made once per isolate:
/// `Private::for_api` looked each one up by name -- a V8 string and a registry probe -- twice per
/// `add` / `remove` / `toggle` / `contains`, which took `classList.add` above its P0 baseline.
/// `for_api` answers the same private for the same name, so this is the key `make_token_list` set.
/// (`pub(crate)`: the snapshot maker releases this isolate slot)
pub(crate) struct DtlKeys {
    id: v8::Global<v8::Private>,
    attr: v8::Global<v8::Private>,
}

fn dtl_keys<'s>(scope: &mut v8::PinScope<'s, '_>) -> (v8::Local<'s, v8::Private>, v8::Local<'s, v8::Private>) {
    if let Some(k) = scope.get_slot::<DtlKeys>() {
        return (v8::Local::new(scope, &k.id), v8::Local::new(scope, &k.attr));
    }
    let id_name = v8::String::new(scope, DTL_ID).unwrap();
    let id = v8::Private::for_api(scope, Some(id_name));
    let attr_name = v8::String::new(scope, DTL_ATTR).unwrap();
    let attr = v8::Private::for_api(scope, Some(attr_name));
    let keys = DtlKeys { id: v8::Global::new(scope, id), attr: v8::Global::new(scope, attr) };
    scope.set_slot(keys);
    (id, attr)
}

fn dtl_id_of(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Option<u32> {
    let (key, _) = dtl_keys(scope);
    this.get_private(scope, key).and_then(|v| v.uint32_value(scope))
}
/// Which content attribute this token list is a view of.
///
/// `classList` is only the most familiar one: `link.relList`, `iframe.sandbox`,
/// `output.htmlFor` and the rest are the same interface over a different attribute. The name is
/// stamped on the list object when it is created (see `cb_el_token_list` in `element.rs`); a list
/// created before this existed has no slot, and `class` is what it meant.
pub(crate) fn dtl_attr_of(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> String {
    let (_, key) = dtl_keys(scope);
    match this.get_private(scope, key) {
        Some(v) if v.is_string() => v.to_rust_string_lossy(scope),
        _ => "class".to_string(),
    }
}


/// Validate a DOMTokenList token for add/remove/toggle/replace: an empty token is a SyntaxError and a
/// token containing ASCII whitespace is an InvalidCharacterError. Throws and returns false when invalid
/// so the caller aborts before mutating anything.
fn dtl_valid_token(scope: &mut v8::PinScope, token: &str) -> bool {
    if token.is_empty() { throw_dom(scope, "The token provided must not be empty.", "SyntaxError"); return false; }
    if token.chars().any(|c| c.is_ascii_whitespace()) { throw_dom(scope, "The token provided contains HTML space characters.", "InvalidCharacterError"); return false; }
    true
}

/// The receiver's element id and the attribute it is a view of ([`dtl_attr_of`]), the name read into `buf` --
/// both privates through one key lookup, and no heap `String` for `class`. `None` for a foreign receiver,
/// whose attribute is then never read (`dtl_id_of` first, as every member did).
fn dtl_of<'b>(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, buf: &'b mut [std::mem::MaybeUninit<u8>; 32]) -> Option<(u32, std::borrow::Cow<'b, str>)> {
    let (id_key, attr_key) = dtl_keys(scope);
    let id = this.get_private(scope, id_key).and_then(|v| v.uint32_value(scope))?;
    let attr = match this.get_private(scope, attr_key).map(v8::Local::<v8::String>::try_from) {
        Some(Ok(s)) => s.to_rust_cow_lossy(scope, buf),
        _ => std::borrow::Cow::Borrowed("class"),
    };
    Some((id, attr))
}

/// An argument as WebIDL's DOMString conversion reads it -- the same `ToString` call
/// `to_rust_string_lossy` makes, user `toString` and throw included -- with a string argument (nearly every
/// call) read into `buf` instead of the heap.
fn dtl_arg<'b>(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, buf: &'b mut [std::mem::MaybeUninit<u8>; 64]) -> std::borrow::Cow<'b, str> {
    match v8::Local::<v8::String>::try_from(v) {
        Ok(s) => s.to_rust_cow_lossy(scope, buf),
        Err(_) => v.to_rust_string_lossy(scope).into(),
    }
}

pub(crate) fn cb_dtl_add(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let mut abuf = [std::mem::MaybeUninit::<u8>::uninit(); 32];
    let Some((id, attr)) = dtl_of(scope, args.this(), &mut abuf) else { return };
    let mut adds = Vec::new();
    for i in 0..args.length() { adds.push(args.get(i).to_rust_string_lossy(scope)); }
    for a in &adds { if !dtl_valid_token(scope, a) { return; } }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let value = {
        let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0;
        let mut toks = ordered_set(dom.get_attribute(id, &attr).unwrap_or(""));
        for a in &adds { if !toks.contains(&a.as_str()) { toks.push(a); } }
        toks.join(" ")
    };
    // Write class via the notify choke point so an `attributes` MutationObserver sees the record
    // (dtl_write went straight to the arena and stayed invisible to observers -> WPT timeouts).
    set_attribute_notify(scope, &op_state, id, &attr, &value);
}
pub(crate) fn cb_dtl_remove(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let mut abuf = [std::mem::MaybeUninit::<u8>::uninit(); 32];
    let Some((id, attr)) = dtl_of(scope, args.this(), &mut abuf) else { return };
    let mut rem = Vec::new();
    for i in 0..args.length() { rem.push(args.get(i).to_rust_string_lossy(scope)); }
    for r in &rem { if !dtl_valid_token(scope, r) { return; } }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let value = {
        let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0;
        let current = dom.get_attribute(id, &attr);
        let toks: Vec<&str> = ordered_set(current.unwrap_or("")).into_iter().filter(|t| !rem.iter().any(|r| r.as_str() == *t)).collect();
        // DOMTokenList update steps: don't CREATE the class attribute when it is absent and the resulting
        // token set is empty (`remove()` on an element with no class must leave it with no class, not "").
        (current.is_some() || !toks.is_empty()).then(|| toks.join(" "))
    };
    if let Some(value) = value { set_attribute_notify(scope, &op_state, id, &attr, &value); }
}
pub(crate) fn cb_dtl_toggle(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let mut abuf = [std::mem::MaybeUninit::<u8>::uninit(); 32];
    let Some((id, attr)) = dtl_of(scope, args.this(), &mut abuf) else { return };
    let mut tbuf = [std::mem::MaybeUninit::<u8>::uninit(); 64];
    let token = dtl_arg(scope, args.get(0), &mut tbuf);
    if !dtl_valid_token(scope, &token) { return; }
    let force_arg = args.get(1);
    let has_force = !force_arg.is_undefined();
    let force = force_arg.boolean_value(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (present, value) = {
        let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0;
        let mut toks = ordered_set(dom.get_attribute(id, &attr).unwrap_or(""));
        let has = toks.contains(&&*token);
        // toggle(token, force) is a no-op when the token is present and force=true, or absent and
        // force=false. In those cases the class attribute must be left BYTE-FOR-BYTE as it was (its
        // whitespace/duplicates preserved) -- only an actual add/remove re-serializes the token set,
        // and only then is the new value built.
        if has {
            if has_force && force { (true, None) } else { toks.retain(|t| *t != &*token); (false, Some(toks.join(" "))) }
        } else if has_force && !force { (false, None) } else { toks.push(&token); (true, Some(toks.join(" "))) }
    };
    if let Some(value) = value { set_attribute_notify(scope, &op_state, id, &attr, &value); }
    rv.set_bool(present);
}
pub(crate) fn cb_dtl_contains(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let mut abuf = [std::mem::MaybeUninit::<u8>::uninit(); 32];
    let Some((id, attr)) = dtl_of(scope, args.this(), &mut abuf) else { rv.set_bool(false); return; };
    let mut tbuf = [std::mem::MaybeUninit::<u8>::uninit(); 64];
    let token = dtl_arg(scope, args.get(0), &mut tbuf);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // Membership needs no de-duplication: a token is in the ordered set exactly when it is in the split.
    let st = op_state.borrow(); rv.set_bool(st.borrow::<HostDom>().0.get_attribute(id, &attr).is_some_and(|s| s.split_whitespace().any(|t| t == token)));
}
pub(crate) fn cb_dtl_replace(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let mut abuf = [std::mem::MaybeUninit::<u8>::uninit(); 32];
    let Some((id, attr)) = dtl_of(scope, args.this(), &mut abuf) else { rv.set_bool(false); return; };
    let (mut obuf, mut nbuf) = ([std::mem::MaybeUninit::<u8>::uninit(); 64], [std::mem::MaybeUninit::<u8>::uninit(); 64]);
    let old = dtl_arg(scope, args.get(0), &mut obuf);
    let new = dtl_arg(scope, args.get(1), &mut nbuf);
    // Validation order per spec: BOTH tokens are checked for empty (SyntaxError) before either is
    // checked for whitespace (InvalidCharacterError) -- so replace(" ", "") reports the empty newToken.
    if old.is_empty() || new.is_empty() { throw_dom(scope, "The token provided must not be empty.", "SyntaxError"); return; }
    if old.chars().any(|c| c.is_ascii_whitespace()) || new.chars().any(|c| c.is_ascii_whitespace()) { throw_dom(scope, "The token provided contains HTML space characters.", "InvalidCharacterError"); return; }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let updated = {
        let st = op_state.borrow(); let dom = &st.borrow::<HostDom>().0;
        let toks = ordered_set(dom.get_attribute(id, &attr).unwrap_or(""));
        if !toks.iter().any(|t| *t == old) { None } // token not in the set -> replace() returns false
        else {
            // Infra "replace within an ordered set": the first token equal to `old` OR `new` becomes
            // `new`, and every other token equal to `old`/`new` is dropped -- so a pre-existing `new`
            // dedups ("a b c".replace("c","a") -> "a b", not "a b a").
            let mut result: Vec<&str> = Vec::with_capacity(toks.len());
            let mut placed = false;
            for t in toks {
                if t == old || t == new { if !placed { result.push(&new); placed = true; } }
                else { result.push(t); }
            }
            Some(result.join(" "))
        }
    };
    match updated { Some(value) => { set_attribute_notify(scope, &op_state, id, &attr, &value); rv.set_bool(true); } None => rv.set_bool(false) }
}
/// The supported-token sets, per REFLECTED ATTRIBUTE and, for `rel`, per element.
///
/// The old body threw TypeError for every list. That half is right and is kept -- `classList` has no
/// supported-token definition, so `el.classList.supports("x")` really is a TypeError -- but it was
/// applied to ALL of them, so `link.relList.supports("stylesheet")` threw where the browser answers
/// `true`. The constant-answers axis reported it as one answer for every input, which it was.
///
/// Every entry below is what Brave 151 on this machine answers, probed rather than read off a spec:
///   <link rel>       stylesheet preload prefetch icon alternate dns-prefetch modulepreload manifest
///                    (and NOT noopener/noreferrer -- those are the hyperlink set)
///   <a|area|form rel> noopener noreferrer opener   (and NOT stylesheet/preload/...)
///   <iframe sandbox> allow-scripts and the rest of the sandbox keywords
/// The two `rel` sets are disjoint, which is the whole reason this cannot be one table.
fn supported_tokens(attr: &str, tag: &str) -> Option<&'static [&'static str]> {
    const LINK_REL: &[&str] = &[
        "alternate", "canonical", "dns-prefetch", "expect", "icon", "manifest", "modulepreload",
        "preconnect", "prefetch", "preload", "stylesheet",
    ];
    // <a>, <area> and <form> share the hyperlink set; Chrome answers the same three for all three.
    const LINK_HYPERLINK_REL: &[&str] = &["noopener", "noreferrer", "opener"];
    const SANDBOX: &[&str] = &[
        "allow-downloads", "allow-forms", "allow-modals", "allow-orientation-lock",
        "allow-pointer-lock", "allow-popups", "allow-popups-to-escape-sandbox", "allow-presentation",
        "allow-same-origin", "allow-scripts", "allow-top-navigation",
        "allow-top-navigation-by-user-activation", "allow-top-navigation-to-custom-protocols",
    ];
    match attr {
        "rel" => Some(match tag {
            "link" => LINK_REL,
            _ => LINK_HYPERLINK_REL,
        }),
        "sandbox" => Some(SANDBOX),
        _ => None,
    }
}

pub(crate) fn cb_dtl_supports(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let attr = dtl_attr_of(scope, args.this());
    let tag = dtl_id_of(scope, args.this())
        .and_then(|id| {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            st.borrow::<HostDom>().0.tag_name(id).map(|t| t.to_ascii_lowercase())
        })
        .unwrap_or_default();
    let Some(set) = supported_tokens(&attr, &tag) else {
        // No supported-token definition for this attribute -- `classList` is the common one. The
        // spec says TypeError, and so does the browser.
        // V8's own TypeError, never the page's replacement.
        let m = v8::String::new(scope, "'supports' is not applicable to this DOMTokenList").unwrap();
        let exc = v8::Exception::type_error(scope, m);
        scope.throw_exception(exc);
        return;
    };
    // The token is ASCII-lowercased before the comparison, per the spec's "supports" steps.
    let t = args.get(0).to_rust_string_lossy(scope).to_ascii_lowercase();
    rv.set_bool(set.contains(&t.as_str()));
}
pub(crate) fn cb_dtl_value_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let mut abuf = [std::mem::MaybeUninit::<u8>::uninit(); 32];
    let Some((id, attr)) = dtl_of(scope, args.this(), &mut abuf) else { let e = str_val(scope, ""); rv.set(e); return; };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let s = { let st = op_state.borrow(); st.borrow::<HostDom>().0.get_attribute(id, &attr).unwrap_or("").to_string() };
    let v = str_val(scope, &s); rv.set(v);
}
pub(crate) fn cb_dtl_value_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let mut abuf = [std::mem::MaybeUninit::<u8>::uninit(); 32];
    let Some((id, attr)) = dtl_of(scope, args.this(), &mut abuf) else { return };
    let val = args.get(0).to_rust_string_lossy(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut(); st.borrow_mut::<HostDom>().0.set_attribute(id, &attr, &val);
}

pub(crate) fn cb_dtl_to_string(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let mut abuf = [std::mem::MaybeUninit::<u8>::uninit(); 32];
    let Some((id, attr)) = dtl_of(scope, args.this(), &mut abuf) else { let e = str_val(scope, ""); rv.set(e); return; };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let s = { let st = op_state.borrow(); st.borrow::<HostDom>().0.get_attribute(id, &attr).unwrap_or("").to_string() };
    let v = str_val(scope, &s); rv.set(v);
}

// No `op_*` function builders here any more. Every one of these bodies is mounted by
// `DOMTokenList`'s `interface!` table (core/dom/collections.rs `install_dom_token_list`), which calls
// the raw callbacks directly -- an op builder existed only so `js/dom/08h-abort-attrs.js` could name
// them from JS, and that install block is gone. `item`, `length` and `entries`/`keys`/`values`/
// `forEach` are not here either: the first two are the shared list members every one of the ten
// carries, and the last four are `Array.prototype`'s OWN function objects, which is what Chrome's
// `DOMTokenList.prototype` holds (`DOMTokenList.prototype.forEach === Array.prototype.forEach`).

// ── moved out of bindings.rs (STEP 3 dissolution) ──
pub(crate) const DTL_ID: &str = "korvus_dtl_id";
pub(crate) const DTL_ATTR: &str = "korvus_dtl_attr";
pub(crate) fn dtl_tokens(dom: &Dom, id: u32, attr: &str) -> Vec<String> {
    ordered_set(dom.get_attribute(id, attr).unwrap_or("")).into_iter().map(str::to_string).collect()
}

/// A token list's attribute value as DOMTokenList's ORDERED SET: whitespace-split with duplicates removed
/// (first wins), borrowed from `s`. The previous split kept duplicates, so `class="a"` reported length 2 /
/// iterated twice instead of the one deduplicated token (WPT Element-classlist -- hundreds of length/item/contains
/// subtests).
///
/// Duplicates are found by a scan of the tokens kept so far, where they used to go through a `HashSet` of
/// `String`s: a class attribute holds a handful of tokens, and the set's allocation and SipHash per token were a
/// fifth of `classList.toggle` (2026-10-23, release profile, with the per-token `String`s). Past
/// [`SCAN_LIMIT`] kept tokens the set comes back, so a long attribute stays linear; the answer is the same either way.
///
/// The split is `str::split_whitespace`, UNICODE whitespace, as it was. Chrome splits on ASCII whitespace only
/// (`"a\u00a0b"` is one token there, two here); that is a known difference left for a change of its own, together
/// with `korvus_dom`'s `class_tokens`, which selectors and `getElementsByClassName` read.
pub(crate) fn ordered_set(s: &str) -> Vec<&str> {
    const SCAN_LIMIT: usize = 32;
    let mut out: Vec<&str> = Vec::new();
    let mut seen: Option<std::collections::HashSet<&str>> = None;
    for t in s.split_whitespace() {
        let fresh = match seen.as_mut() {
            Some(set) => set.insert(t),
            None => !out.contains(&t),
        };
        if fresh {
            out.push(t);
            if seen.is_none() && out.len() > SCAN_LIMIT {
                seen = Some(out.iter().copied().collect());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::ordered_set;

    /// The `HashSet` de-duplication `ordered_set` replaced: the reference it must agree with.
    fn reference(s: &str) -> Vec<&str> {
        let mut seen = std::collections::HashSet::new();
        s.split_whitespace().filter(|t| seen.insert(*t)).collect()
    }

    #[test]
    fn ordered_set_is_the_first_wins_dedup() {
        let long: String = (0..200).map(|i| format!("t{} ", i % 70)).collect();
        let wide: String = (0..40).map(|i| format!("w{i} a ")).collect();
        for s in ["", "   ", "a", "a a b", "b a b a c", "  a\tb\nc  ", "a\u{a0}b c", "\u{3000}x y", "a\u{b}b", "a\u{c}b\rc", &long, &wide] {
            assert_eq!(ordered_set(s), reference(s), "{s:?}");
        }
    }
}
