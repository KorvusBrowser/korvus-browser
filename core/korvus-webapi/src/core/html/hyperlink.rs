//! `HTMLHyperlinkElementUtils` — the URL components of an `<a>` or `<area>`'s `href`.
//!
//! `a.protocol`, `a.host`, `a.pathname` and the rest are not attributes; they are pieces of the
//! element's href PARSED AS A URL, against the document's base URL. There is no honest way to expose
//! them without a real parser, so this uses the same `url` crate the runtime's Location does.
//!
//! The spec's null-URL branch is the one a detached `<a>` with no href exercises, and it is the one
//! the recorded corpus captured: every component is "" except `protocol`, which is ":". Implementing
//! only that branch would pass every recorded case while parsing nothing -- so the branch falls out
//! of the parse here rather than being special-cased into existence.
//!
//! Setters re-serialize: each mutates its component and writes the whole URL back to `href`, which is
//! what makes `a.protocol = "https"` show up in `a.href`. A setter on an element whose href does not
//! parse is a no-op, as the spec says ("if this's url is null, then return").

use deno_core::v8;

use crate::binding::HostDom;
use crate::binding::protos::{arg_node_id, build_fn_data, get_prop, str_val, throw_type_error};
use crate::core::dom::element::el_id_of;
use crate::install::MemberDef;
use crate::modules::url::components;

/// WHATWG URL's *query percent-encode set*: the C0-control set (C0 controls and everything above 0x7E)
/// plus SPACE, `"`, `#`, `<`, `>`. Special schemes (http/https/ws/wss/ftp/file) add `'`.
fn in_query_percent_encode_set(b: u8, special: bool) -> bool {
    b < 0x20 || b > 0x7E || matches!(b, b' ' | b'"' | b'#' | b'<' | b'>') || (special && b == b'\'')
}

/// WHATWG URL's *percent-encode after encoding*, run over a query string with the document's encoding
/// as the override.
///
/// This is the whole reason `DocEncoding` exists. The URL parser does NOT percent-encode a query's
/// non-ASCII as UTF-8 when the document declares another encoding -- it encodes the code points to
/// BYTES in that encoding first, then percent-encodes the bytes. `?한` is `%C7%D1` in an EUC-KR
/// document and `%ED%95%9C` in a UTF-8 one, and both are correct for their document.
///
/// An UNMAPPABLE code point is the part worth reading the spec twice for. It does not drop and does not
/// become U+FFFD; the spec appends the literal text `%26%23`, the decimal code point, and `%3B` -- that
/// is, the ALREADY-percent-encoded form of `&#N;`. It does not emit `&#N;` and run it through the
/// percent-encode set, which would leave `&` and `;` bare (`&` and `;` are not in the query set) and
/// produce `&%2321707;`. That distinction is the entire difference between the `-errors-` WPT variants
/// passing and failing, and it is why this drives `encoding_rs`'s encoder by hand instead of calling
/// `Encoding::encode`, whose HTML error mode emits the bare `&#N;`.
///
/// The encoder is created once and driven across the whole string because ISO-2022-JP is STATEFUL: it
/// carries an escape-sequence mode between characters, so encoding chunk by chunk with fresh encoders
/// would emit a mode switch per chunk and corrupt the output.
///
/// UTF-8 documents take this same path and get the same answer as before; there is no fast path,
/// because a second implementation is a second thing to keep in agreement.
fn encode_query(q: &str, enc: &'static encoding_rs::Encoding, special: bool) -> String {
    use encoding_rs::EncoderResult;

    let mut out = String::with_capacity(q.len());
    let mut encoder = enc.new_encoder();
    let mut input = q;
    let mut buf = [0u8; 512];
    loop {
        let (result, read, written) = encoder.encode_from_utf8_without_replacement(input, &mut buf, true);
        for &b in &buf[..written] {
            if in_query_percent_encode_set(b, special) {
                out.push_str(&format!("%{:02X}", b));
            } else {
                out.push(b as char);
            }
        }
        input = &input[read..];
        match result {
            EncoderResult::InputEmpty => break,
            EncoderResult::OutputFull => continue,
            // The spec's own spelling, pre-encoded. See the note above.
            EncoderResult::Unmappable(c) => out.push_str(&format!("%26%23{}%3B", c as u32)),
        }
    }
    out
}

/// Split a href into (everything before the query, raw query, fragment) so the query can be re-encoded
/// under the document's encoding before the URL crate -- which is UTF-8-only, rust-url dropped
/// `encoding_override` in 2.x -- ever sees it. The fragment is NOT re-encoded: the spec applies the
/// override to the query alone, and a fragment is always UTF-8.
fn split_query(href: &str) -> Option<(&str, &str, &str)> {
    let q = href.find('?')?;
    // A '?' after a '#' is part of the fragment, not a query.
    if let Some(h) = href.find('#') {
        if h < q {
            return None;
        }
    }
    let rest = &href[q + 1..];
    let (query, frag) = match rest.find('#') {
        Some(h) => (&rest[..h], &rest[h..]),
        None => (rest, ""),
    };
    Some((&href[..q], query, frag))
}

/// The element's href parsed against the document base URL, or `None` when there is no href or it
/// does not parse -- the spec's "url is null".
fn href_url(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> Option<url::Url> {
    let id = arg_node_id(scope, this)?;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let href = op_state.borrow().borrow::<HostDom>().0.get_attribute(id, "href")?.to_string();
    // The base is the document's BASE url -- `<base href>` applied. It was the document
    // url until then, so `<base href="/sub/">` moved `a.href` in Chrome and not here. The element's
    // OWN document's: a link in a frame resolves against the frame (`subresource::node_base_url`).
    let base = crate::core::html::subresource::node_base_url(scope, id);
    // The link's OWN document's encoding -- a frame's, for a link in a frame.
    let enc = crate::core::html::browsing_context::node_document_encoding(scope, id);

    // Apply the document's encoding override to the query before parsing. rust-url would percent-encode
    // the raw non-ASCII as UTF-8 and there is no hook to change that, so the query is encoded here and
    // handed over as pure ASCII -- which the parser then passes through unchanged.
    let href = match split_query(&href) {
        Some((head, query, frag)) if !query.is_empty() => {
            // `special` follows the scheme of the URL this href will RESOLVE to, which for a relative
            // href is the base's. Resolving twice is the honest way to know it and costs one parse of a
            // string we are about to parse anyway.
            let special = components::resolve(&base, &href)
                .map(|u| matches!(u.scheme(), "http" | "https" | "ws" | "wss" | "ftp" | "file"))
                .unwrap_or(false);
            format!("{head}?{}{frag}", encode_query(query, enc, special))
        }
        _ => href,
    };

    components::resolve(&base, &href)
}

/// Write a mutated URL back to `href`, which is where every setter's effect actually lives.
fn set_href(scope: &mut v8::PinScope, this: v8::Local<v8::Value>, u: &url::Url) {
    let Some(id) = arg_node_id(scope, this) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    op_state.borrow_mut().borrow_mut::<HostDom>().0.set_attribute(id, "href", u.as_str());
}

// EACH MACRO USED TO EMIT AN `#[op2]` BUILDER BESIDE ITS CALLBACK -- `op_hl_protocol_get` and its
// eighteen siblings, whose only job was to hand a JS install file a native function to
// `Object.defineProperty` onto a prototype. `<a>` stopped calling them at W5.1 L1 and `<area>` at
// W5.1 L14, which left all nineteen with no caller in any realm: a table row names the CALLBACK
// directly (`install_html_anchor_element`, `install_html_area_element`), so the round trip through an
// op no longer exists. They are deleted, and `tests/fixtures/ops.manifest` records the smaller
// registry.

/// A component getter: `|url| String`, with the spec's null-URL answer when there is no URL.
macro_rules! url_get {
    ($cb:ident, $null:expr, |$u:ident| $read:expr) => {
        pub(crate) fn $cb(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
            let s = match href_url(scope, args.this().into()) {
                Some($u) => $read,
                None => $null.to_string(),
            };
            let v = str_val(scope, &s);
            rv.set(v);
        }
    };
}

/// A component setter: mutate and write back. A no-op when the href does not parse.
macro_rules! url_set {
    ($cb:ident, |$u:ident, $val:ident| $write:expr) => {
        pub(crate) fn $cb(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
            let this: v8::Local<v8::Value> = args.this().into();
            let Some(mut $u) = href_url(scope, this) else { return };
            let $val = args.get(0).to_rust_string_lossy(scope);
            let ok: bool = $write;
            if ok {
                set_href(scope, this, &$u);
            }
        }
    };
}

// EVERY BODY BELOW IS `crate::modules::url::components`, WHICH IS `url::quirks`, WHICH IS THE SPEC.
// This file used to carry its own nine getters and its own nine setters, and `modules/url/url.rs`
// carried a SECOND, independently-wrong set of the same nine -- which is why `url-setters.any.js`
// and `url-setters-a-area.window.js` failed the same rows for different reasons. One algorithm, one
// copy; the only thing that stays here is the null-URL answer, which is this mixin's and not `URL`'s.
//
// `protocol` is the one component whose null answer is not "": the getter is defined as the scheme
// followed by ':', and an absent scheme still carries its colon.
url_get!(cb_hl_protocol_get, ":", |u| components::protocol(&u));
url_get!(cb_hl_username_get, "", |u| components::username(&u));
url_get!(cb_hl_password_get, "", |u| components::password(&u));
url_get!(cb_hl_hostname_get, "", |u| components::hostname(&u));
url_get!(cb_hl_port_get, "", |u| components::port(&u));
url_get!(cb_hl_pathname_get, "", |u| components::pathname(&u));
url_get!(cb_hl_search_get, "", |u| components::search(&u));
url_get!(cb_hl_hash_get, "", |u| components::hash(&u));
url_get!(cb_hl_host_get, "", |u| components::host(&u));
// `origin` is the SERIALIZED origin, which is "null" for a scheme that has none (file:, data:) --
// distinct from the empty string a null URL gives.
url_get!(cb_hl_origin_get, "", |u| components::origin(&u));

url_set!(cb_hl_protocol_set, |u, v| components::set_protocol(&mut u, &v));
url_set!(cb_hl_username_set, |u, v| components::set_username(&mut u, &v));
url_set!(cb_hl_password_set, |u, v| components::set_password(&mut u, &v));
url_set!(cb_hl_hostname_set, |u, v| components::set_hostname(&mut u, &v));
url_set!(cb_hl_port_set, |u, v| components::set_port(&mut u, &v));
url_set!(cb_hl_pathname_set, |u, v| components::set_pathname(&mut u, &v));
url_set!(cb_hl_search_set, |u, v| components::set_search(&mut u, &v));
url_set!(cb_hl_hash_set, |u, v| components::set_hash(&mut u, &v));
url_set!(cb_hl_host_set, |u, v| components::set_host(&mut u, &v));

// ── the stringifier, ONE definition for both includers ─────────────────────────────────────────

/// `HTMLHyperlinkElementUtils`'s `stringifier` over `href`: `String(el)` and `el.toString()` are the
/// `href` GETTER, so a relative markup href stringifies absolute. Both `<a>` and `<area>` include the
/// mixin and Chrome puts an OWN `toString` (length 0) on each prototype -- ONE algorithm, so ONE body
/// here, named by `core/html/html_anchor_element.rs`'s row and by
/// `core/html/html_area_element.rs`'s (the first was written for `<a>`; `<area>` shares it instead of
/// writing a second copy).
///
/// THE RECEIVER CHECK IS AIMED AT THE INTERFACE, NOT AT THE MIXIN, which is why the element's TAG
/// travels as the callback's data. Measured in Chrome 150 over loopback:
///
/// ```text
/// HTMLAreaElement.prototype.toString.call({})                  -> TypeError: Illegal invocation
/// HTMLAnchorElement.prototype.toString.call({})                -> TypeError: Illegal invocation
/// HTMLAreaElement.prototype.toString.call(document.createElement("a"))
///                                                              -> TypeError: Illegal invocation
/// ```
///
/// The last line is the one a shared body gets wrong by default: an `<a>` HAS an `href`, so a body
/// that only reads `this.href` answers the anchor's URL where Chrome refuses. Before this lane the
/// engine answered `""` for the first two and the anchor's href for the third.
pub(crate) fn cb_hl_to_string(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let tag = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let this = args.this();
    let ok = el_id_of(scope, this).is_some()
        && crate::page::tag_name_of(scope, (this).into()).is_some_and(|t| t.eq_ignore_ascii_case(&tag));
    if !ok {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    let s = match get_prop(scope, this, "href") {
        Some(v) if !v.is_null_or_undefined() => v.to_rust_string_lossy(scope),
        _ => String::new(),
    };
    let v = str_val(scope, &s);
    rv.set(v);
}

/// Mount the stringifier row on `proto`, aimed at elements whose tag name is `tag`.
pub(crate) fn to_string_row(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef, tag: &str) {
    let d = str_val(scope, tag);
    let f = build_fn_data(scope, cb_hl_to_string, member.name, member.length as i32, d);
    crate::install::kits::define_value(scope, proto, member, f.into(), true);
}

