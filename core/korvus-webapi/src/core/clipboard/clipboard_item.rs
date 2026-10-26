//! `core/clipboard/clipboard_item.rs` -- `ClipboardItem` as one `interface!` table.
//!
//! Three rows and the constructor. `getType` stays an honest `stub` (there is no clipboard behind it and a
//! fabricated answer would be a `constant-answers` tell); `supports` is the static a feature-detecting
//! page actually calls, and it is the reason this block was in `dom/09-postshells.js`: the generated
//! shell gave it the right SHAPE and no behaviour, so it answered `undefined` -- falsy -- to every
//! media type, which reads as "no clipboard type is supported at all".
//!
//! `types` IS THE PAGE'S OWN KEYS (C2, 2026-10-11), not a clipboard read: `new ClipboardItem({...}).types` is the
//! record's keys in property order, a new frozen array per read (`FrozenArray<DOMString>`, the getter returns a
//! `Vector`) -- it answered `undefined`, a FrozenArray attribute Chrome exposes and korvus did not. So the
//! constructor is real now: Blink's record conversion and its refusals, measured on Chrome 150 (three runs alike,
//! `core/korvus-runtime/tests/fixtures/frozen-arrays/`):
//!
//! ```text
//!   new ClipboardItem(5)                  TypeError  Failed to construct 'ClipboardItem': Only objects can be converted to record<K,V> types
//!   new ClipboardItem({})                 TypeError  ...: Empty dictionary argument          (non-enumerable keys do not count)
//!   a Symbol key                          TypeError  ...: Cannot convert a Symbol value to a string
//!   a throwing getter                     its own error, unwrapped
//!   {'2':a, '1':b, z:c}                   types 1|2|z              (OwnPropertyKeys: indices first)
//!   'web  text/y '                        'web text/y'             (a `web ` format re-spelled, see `representation_type`;
//!   'web  bad', 'web x/y; q=1', 'TEXT/HTML'  kept as given)
//! ```
//!
//! WHAT `supports` ANSWERS, recorded from Chrome 150 over a loopback page on 2026-09-11 across
//! nineteen inputs. The rule is two clauses and no coercion:
//!
//! ```text
//!   text/plain  text/html  image/png  image/svg+xml   -> true    (the mandatory data types, EXACT)
//!   "web " + a valid MIME type                        -> true    ("web x/y", "web image/jpeg")
//!   everything else                                   -> false
//! ```
//!
//! FOUR THINGS THE PREVIOUS IMPLEMENTATION GOT WRONG, all four measured:
//!
//! * `"TEXT/PLAIN"` -> it answered TRUE (it lowercased first); Chrome answers false.
//! * `" text/plain "` -> it answered TRUE (it trimmed); Chrome answers false.
//! * `"web x/y"` -> it answered FALSE (it stripped `web ` and then required a MANDATORY type);
//!   Chrome answers true -- the whole point of the `web ` prefix is that it admits CUSTOM formats.
//! * `"web image/jpeg"` -> same, false where Chrome says true.
//!
//! `"web "`, `"web"`, `"web foo"` and `"web a/b/c"` are all false: the remainder after the prefix has
//! to be a MIME type, which is exactly one `/` with a non-empty type and subtype.
//!
//! The bundle's oracle records one case (`supports("x") == false`), which every version above
//! satisfies -- which is why this needed Chrome, not the fixture.

use deno_core::v8;

use crate::binding::protos::{require_args, str_val, throw_type_error};
use crate::core::dom::shared::row_method;
use crate::install::MemberDef;

/// The Clipboard API's mandatory data types, compared EXACTLY: no trim, no case folding.
const MANDATORY: [&str; 4] = ["text/plain", "text/html", "image/png", "image/svg+xml"];

/// Is `s` a MIME type -- exactly one `/`, with a non-empty type and subtype? The `web ` clause's test.
fn is_mime(s: &str) -> bool {
    let mut it = s.split('/');
    match (it.next(), it.next(), it.next()) {
        (Some(t), Some(sub), None) => !t.is_empty() && !sub.is_empty(),
        _ => false,
    }
}

/// `ClipboardItem.supports(type)`, as Chrome 150 answers it.
pub(crate) fn supports(ty: &str) -> bool {
    if MANDATORY.contains(&ty) {
        return true;
    }
    match ty.strip_prefix("web ") {
        Some(rest) => is_mime(rest),
        None => false,
    }
}

fn cb_ci_supports<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    // A static: there is no receiver to check, and Chrome answers for `supports.call(null, t)`.
    if !require_args(scope, &args, 1, "ClipboardItem", "supports") {
        return;
    }
    let ty = args.get(0).to_rust_string_lossy(scope);
    rv.set_bool(supports(&ty));
}

/// The private slot holding a constructed item's types, as the JS strings `types` hands back.
const CI_TYPES: &str = "korvus_clipboard_item_types";

/// base's `kWhitespaceASCII`, which `TrimWhitespaceASCII` trims.
fn ascii_ws(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\x0B' | '\x0C' | '\r' | ' ')
}

/// An HTTP token (RFC 7230 3.2.6, `net::HttpUtil::IsToken`): non-empty, every byte visible ASCII and no separator.
fn is_token(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| (0x21..0x7F).contains(&b) && !b"()<>@,;:\\\"/[]?={}".contains(&b))
}

/// The type a representation is kept under -- Blink's `ClipboardItem` constructor over
/// `Clipboard::ParseWebCustomFormat`: a `web ` key whose rest is `type/subtype` (two HTTP tokens, the first's
/// leading and the second's trailing ASCII whitespace dropped: `net::ParseMimeTypeWithoutParameter`) is spelled
/// `web type/subtype`; every other key is kept as given. `None`: keep the key itself.
fn representation_type(key: &str) -> Option<String> {
    let rest = key.strip_prefix("web ")?;
    let mut parts = rest.split('/');
    let (Some(t), Some(s), None) = (parts.next(), parts.next(), parts.next()) else { return None };
    let (t, s) = (t.trim_start_matches(ascii_ws), s.trim_end_matches(ascii_ws));
    if !is_token(t) || !is_token(s) {
        return None;
    }
    let spelled = format!("web {t}/{s}");
    (spelled != key).then_some(spelled)
}

/// `record<DOMString, ClipboardItemData>`, converted as Blink's `NativeValueTraits<IDLRecord>` converts it: the
/// object's own keys in `[[OwnPropertyKeys]]` order (symbols included), each one's descriptor asked, an enumerable
/// one's key converted to a DOMString and its value read and made a promise (`ClipboardItemData` is a
/// `Promise<(DOMString or Blob)>`: a promise kept, anything else resolved -- the resolve reads an object's `then`).
/// What comes back is the representation types, in order; `None` when a step threw.
fn representation_types<'s>(scope: &mut v8::PinScope<'s, '_>, items: v8::Local<'s, v8::Value>) -> Option<Vec<v8::Local<'s, v8::Value>>> {
    const AT: &str = "Failed to construct 'ClipboardItem': ";
    let Ok(o) = v8::Local::<v8::Object>::try_from(items) else {
        throw_type_error(scope, &format!("{AT}Only objects can be converted to record<K,V> types"));
        return None;
    };
    let keys = o.get_own_property_names(
        scope,
        v8::GetPropertyNamesArgs {
            mode: v8::KeyCollectionMode::OwnOnly,
            property_filter: v8::PropertyFilter::ALL_PROPERTIES,
            index_filter: v8::IndexFilter::IncludeIndices,
            key_conversion: v8::KeyConversionMode::ConvertToString,
        },
    )?;
    let enumerable_k = str_val(scope, "enumerable");
    let mut out = Vec::new();
    for i in 0..keys.length() {
        let key = keys.get_index(scope, i)?;
        let Ok(name) = v8::Local::<v8::Name>::try_from(key) else { continue };
        let desc = o.get_own_property_descriptor(scope, name)?;
        let Ok(desc) = v8::Local::<v8::Object>::try_from(desc) else { continue };
        if !desc.get(scope, enumerable_k).is_some_and(|e| e.boolean_value(scope)) {
            continue;
        }
        if key.is_symbol() {
            throw_type_error(scope, &format!("{AT}Cannot convert a Symbol value to a string"));
            return None;
        }
        let value = o.get(scope, key)?;
        if !value.is_promise() {
            let resolver = v8::PromiseResolver::new(scope)?;
            resolver.resolve(scope, value)?;
        }
        let text = key.to_rust_string_lossy(scope);
        out.push(match representation_type(&text) {
            Some(spelled) => str_val(scope, &spelled),
            None => key,
        });
    }
    Some(out)
}

/// `new ClipboardItem(items)`: the `new` and argument checks every constructor makes, the record converted, and
/// `ClipboardItem::Create`'s refusal of an empty one ("Empty dictionary argument" -- what a page's promise, which
/// converts to `{}`, would otherwise make). The item keeps its types; the representations themselves have no
/// clipboard to go to.
pub(crate) fn cb_clipboard_item_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    if !crate::binding::protos::require_ctor_new_target(scope, &args, "ClipboardItem")
        || !crate::binding::protos::require_ctor_args(scope, &args, 1, "ClipboardItem")
    {
        return;
    }
    let Some(types) = representation_types(scope, args.get(0)) else { return };
    if types.is_empty() {
        throw_type_error(scope, "Failed to construct 'ClipboardItem': Empty dictionary argument");
        return;
    }
    let list = v8::Array::new_with_elements(scope, &types);
    crate::core::dom::element::priv_set(scope, args.this(), CI_TYPES, list.into());
}

/// `readonly attribute FrozenArray<DOMString> types`: the getter returns its `Vector`, so a new frozen array per
/// read (`install::frozen_array`). An object no constructor made (`Object.create(ClipboardItem.prototype)`) is not
/// an item: `Illegal invocation`, as Chrome's receiver check answers.
fn cb_ci_types<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let this = args.this();
    let Some(list) = crate::core::css::css_style_declaration::priv_get(scope, this, CI_TYPES).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) else {
        crate::binding::protos::illegal_invocation(scope);
        return;
    };
    let answer = crate::install::frozen_array::answer(scope, this, "ClipboardItem", "types", |_| 0.0, |scope| {
        Some((0..list.length()).filter_map(|i| list.get_index(scope, i)).collect())
    });
    if let Some(a) = answer {
        rv.set(a);
    }
}

/// The rows with a body. `row_method` mounts `supports` on the INTERFACE OBJECT for a static row, with the
/// capture's `{writable: true, enumerable: true, configurable: true}` and the arity guard its
/// `required 1` asks for -- which is the right order here, because a static has no receiver check to
/// come first.
pub fn install_clipboard_item(scope: &mut v8::PinScope, target: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "supports" => row_method(scope, target, member, cb_ci_supports),
        "types" => crate::install::live::getter_row(scope, target, member, cb_ci_types),
        other => debug_assert!(false, "install_clipboard_item: unexpected member {other}"),
    }
}

webapi::interface! {
    "ClipboardItem" [value, window, secure] {
        ctor "ClipboardItem" len 1 required 1 = stub,
        attr "types" : Any readonly = install_clipboard_item,
        method "getType" : Promise len 1 required 1 = stub,
        static_method "supports" : Any len 1 required 1 = install_clipboard_item
    }
}
