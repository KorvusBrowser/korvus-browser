//! `location.rs` -- the window's `Location` object: every member, and the navigation each setter
//! starts.
//!
//! WHY THE RUNTIME CRATE: the page's URL is `HostLocation` and a cross-document navigation is
//! `webapi::PendingNavigation`, and a same-document navigation moves the session history
//! `singletons.rs` keeps -- the same reason `History` lives here.
//!
//! # What it replaces, measured
//!
//! `JsLocation`'s `#[op2]` members, which `singletons::reshape_location` copied onto the instance.
//! Against stock Chrome 150.0.7871.46, Brave 151.1.93.138, Edge 151.0.4129.93 and Opera 135.0.5973.133
//! over a loopback page (`tests/fixtures/p11a/{desc,location,fragment,nav}.*.txt`; the four agree):
//!
//! ```text
//! protocol host hostname pathname search hash   korvus getter only          browsers get + set
//! port ancestorOrigins valueOf @@toPrimitive     korvus absent               browsers own
//! assign/replace/reload/toString .length         korvus 2 / 2 / 1 / 1        browsers 1 / 1 / 0 / 0
//! location.hash = "a"                            korvus nothing              browsers same-document: URL
//!                                                                             moves, popstate now,
//!                                                                             hashchange on a task
//! location.href = "#c"                           korvus a full reload        browsers same-document
//! location.pathname = "/p2.html"                 korvus nothing              browsers navigate
//! location.assign("http://[")                    korvus navigates            browsers SyntaxError
//! assign.call({}) / get href on {}               korvus "expected JsLocation" browsers Illegal invocation
//! ```
//!
//! # Where the model is thinner than a browser's
//!
//! A cross-document navigation is a `webapi::PageNavigation` the driver loads, and whether it REPLACES the current
//! session-history entry (`replace()`, `reload()`) is not carried to the driver: the next document gets
//! an entry either way. `Object.setPrototypeOf(location, x)` does not throw, because the `Location`
//! object is a `deno_core` class instance and V8's immutable-prototype bit is a property of the
//! object's template (`ObjectTemplate::SetImmutableProto`), not something that can be set on an
//! existing object.

use deno_core::v8;

use webapi::binding::protos::{str_val, template_fn, throw_dom, throw_type_error};

use crate::HostLocation;

/// The window's one `Location`, for the receiver check -- every member of it throws `Illegal
/// invocation` for any other receiver, including another object carrying `Location.prototype`.
pub(crate) struct LocationObject(v8::Global<v8::Object>);

/// Whether the page's `Location` has been minted -- a later realm's install must not mint a second.
pub(crate) fn page_location_minted(scope: &mut v8::PinScope) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let minted = op_state.borrow().has::<LocationObject>();
    minted
}

/// The context snapshot (slice 3): the page's `Location` -- minted by the install, and the one receiver its
/// members accept -- into a snapshot of the page realm: a flag, then its id in the snapshot's store, appended to `out`.
#[cfg(not(feature = "analysis"))]
pub(crate) fn snapshot_location(scope: &mut v8::PinScope, store: &mut deno_core::snapshot::SnapshotStoreDataStore, out: &mut Vec<u8>) {
    let held = deno_core::JsRuntime::op_state_from(scope).borrow().try_borrow::<LocationObject>().map(|l| l.0.clone());
    match held {
        None => out.push(0),
        Some(g) => {
            out.push(1);
            out.extend_from_slice(&store.register(g).to_le_bytes());
        }
    }
}

/// [`snapshot_location`]'s reader: `b` from `*at` on.
#[cfg(not(feature = "analysis"))]
pub(crate) fn restore_location(scope: &mut v8::PinScope, store: &mut deno_core::snapshot::SnapshotLoadDataStore, b: &[u8], at: &mut usize) -> Result<(), String> {
    match b.get(*at) {
        Some(0) => {
            *at += 1;
            Ok(())
        }
        Some(1) => {
            let id = b.get(*at + 1..*at + 5).and_then(|x| x.try_into().ok()).map(u32::from_le_bytes).ok_or("the snapshot's Location does not decode")?;
            *at += 5;
            let g = store.get::<v8::Object>(scope, id);
            deno_core::JsRuntime::op_state_from(scope).borrow_mut().put(LocationObject(g));
            Ok(())
        }
        _ => Err("the snapshot's Location does not decode".to_string()),
    }
}

/// `location.ancestorOrigins`, `[SameObject]`.
struct AncestorOrigins(v8::Global<v8::Value>);

fn recv(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let held = op_state.borrow().try_borrow::<LocationObject>().map(|l| l.0.clone());
    let ok = held.map(|g| v8::Local::new(scope, &g) == this).unwrap_or(false);
    if !ok {
        throw_type_error(scope, "Illegal invocation");
    }
    ok
}

pub(crate) fn current_href(scope: &mut v8::PinScope) -> String {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<HostLocation>().map(|l| l.get()).unwrap_or_default()
}

fn set_href(scope: &mut v8::PinScope, url: &str) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    if let Some(l) = st.try_borrow::<HostLocation>() {
        l.set(url);
    }
}

/// A session-history traversal to an entry of an earlier document (`history.back()`, `navigation.traverseTo`,
/// `Page.navigateToHistoryEntry`): a navigation to its URL whose document takes that entry's place (`serial`), not a
/// new one after the current.
pub(crate) fn request_traversal(scope: &mut v8::PinScope, url: &str, serial: u64) {
    let mut nav = webapi::PageNavigation::get(url, webapi::NavigationReason::ScriptInitiated);
    nav.history = webapi::page::HistoryCommit::Traverse(serial);
    webapi::request_top_navigation(scope, nav);
}

/// A reload of the current document (`location.reload()`, `history.go(0)`): a navigation to its own
/// URL that Chrome requests with `Cache-Control: max-age=0` and reports as the type `reload` -- after its
/// `navigate` event ([`navigate_page`]).
pub(crate) fn request_reload(scope: &mut v8::PinScope) {
    let cur = current_href(scope);
    navigate_page(
        scope,
        webapi::PageNavigate {
            nav: webapi::PageNavigation::reload(cur, webapi::NavigationReason::ScriptInitiated),
            history: webapi::HistoryHandling::Reload,
            reason: webapi::ClientReason::FrameNavigation,
            source_element: None,
            user_initiated: false,
            form_data: None,
        },
    );
}

/// The URL the member reads, parsed. `None` only for a current URL that does not parse at all.
fn current_url(scope: &mut v8::PinScope) -> Option<url::Url> {
    url::Url::parse(&current_href(scope)).ok()
}

/// WebIDL `USVString` conversion of `v`, with the exception (a Symbol) left pending.
fn usv(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<String> {
    v.to_string(scope).map(|s| s.to_rust_string_lossy(scope))
}

/// Parse `input` against the current URL, or throw Chrome's SyntaxError with `prefix` ("Failed to
/// execute 'assign' on 'Location'" / "Failed to set the 'href' property on 'Location'").
fn parse_or_throw(scope: &mut v8::PinScope, input: &str, prefix: &str) -> Option<url::Url> {
    let base = current_url(scope);
    let parsed = match &base {
        Some(b) => b.join(input),
        None => url::Url::parse(input),
    };
    match parsed {
        Ok(u) => Some(u),
        Err(_) => {
            let text = webapi::invalid_url_text(input, base.as_ref());
            throw_dom(scope, &format!("{prefix}: '{text}' is not a valid URL."), "SyntaxError");
            None
        }
    }
}

/// HTML's "Location-object navigate": the page's navigation ([`navigate_page`]) as `location.*` asks for it.
pub(crate) fn navigate(scope: &mut v8::PinScope, url: &url::Url, replace: bool) {
    navigate_page(
        scope,
        webapi::PageNavigate {
            nav: webapi::PageNavigation::get(url.as_str(), webapi::NavigationReason::ScriptInitiated),
            history: if replace { webapi::HistoryHandling::Replace } else { webapi::HistoryHandling::Auto },
            reason: webapi::ClientReason::FrameNavigation,
            source_element: None,
            user_initiated: false,
            form_data: None,
        },
    );
}

/// HTML's "navigate" of the page (Blink `LocalFrame::Navigate` and `FrameLoader::StartNavigation`) -- for `location.*`,
/// `history.go(0)`, and through [`webapi::PageHooks::navigate`] for what `korvus-webapi` starts: `navigation.*`, a link,
/// a form, `window.open(url, "_self")`.
///
/// * A push becomes a REPLACE when the URL is the document's own (not for a form), or -- not for a link, nor for a
///   navigation the user started -- while the document's `load` event has not finished (Blink
///   `NavigationShouldReplaceCurrentHistoryEntry`; HTML: "not yet completely loaded"). An explicit
///   `navigation.navigate(url, {history: "push"})` is never converted.
/// * A `GET` to the document's URL with a fragment is a navigation to a fragment, in this document
///   ([`crate::singletons::fragment_navigate`]); anything else loads a new document, after its `navigate` event --
///   which the page may cancel, or intercept into a same-document navigation.
/// * A `javascript:` URL fires no `navigate` event (Blink runs it before the event is reached).
pub(crate) fn navigate_page(scope: &mut v8::PinScope, p: webapi::PageNavigate) {
    use webapi::{HistoryHandling as H, NavigationType as T};
    let Ok(url) = url::Url::parse(&p.nav.url) else {
        webapi::request_top_navigation(scope, p.nav);
        return;
    };
    if url.scheme().eq_ignore_ascii_case("javascript") {
        webapi::request_top_navigation(scope, p.nav);
        return;
    }
    let current = current_url(scope);
    let is_get = p.nav.method.eq_ignore_ascii_case("GET") && p.nav.body.is_none();
    let mut nav_type = match p.history {
        H::Reload => T::Reload,
        H::Replace => T::Replace,
        H::Push | H::Auto => T::Push,
    };
    if p.history == H::Auto {
        let same_url = current.as_ref() == Some(&url) && p.reason != webapi::ClientReason::FormSubmission;
        let before_load = !webapi::document_load_event_finished(scope)
            && !p.user_initiated
            && !matches!(p.reason, webapi::ClientReason::AnchorClick | webapi::ClientReason::None);
        if same_url || before_load {
            nav_type = T::Replace;
        }
    }
    let same_document = is_get
        && nav_type != T::Reload
        && url.fragment().is_some()
        && current.as_ref().is_some_and(|cur| {
            let mut a = cur.clone();
            a.set_fragment(None);
            let mut b = url.clone();
            b.set_fragment(None);
            a == b
        });
    if same_document {
        crate::singletons::fragment_navigate(scope, url.as_str(), nav_type, p.source_element, p.user_initiated);
        return;
    }
    let mut np = webapi::NavigateParams::new(url.as_str(), webapi::NavigateEventType::CrossDocument, nav_type);
    np.source_element = p.source_element;
    np.user_initiated = p.user_initiated;
    np.form_data = p.form_data;
    if webapi::dispatch_navigate_event(scope, np) != webapi::DispatchResult::Continue {
        return;
    }
    // where the document will enter the session history, decided above as Blink decides it
    let mut nav = p.nav;
    nav.history = match nav_type {
        T::Replace => webapi::page::HistoryCommit::Replace,
        T::Reload => webapi::page::HistoryCommit::Reload,
        _ => webapi::page::HistoryCommit::Push,
    };
    webapi::request_top_navigation(scope, nav);
}

// ── the members ──────────────────────────────────────────────────────────────────────────────────

fn cb_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !recv(scope, args.this()) {
        return;
    }
    let part = webapi::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let href = current_href(scope);
    let s = match url::Url::parse(&href) {
        Err(_) => if part == "href" { href } else { String::new() },
        Ok(u) => match part.as_str() {
            "href" => url::quirks::href(&u).to_string(),
            "origin" => url::quirks::origin(&u),
            "protocol" => url::quirks::protocol(&u).to_string(),
            "host" => url::quirks::host(&u).to_string(),
            "hostname" => url::quirks::hostname(&u).to_string(),
            "port" => url::quirks::port(&u).to_string(),
            "pathname" => url::quirks::pathname(&u).to_string(),
            "search" => url::quirks::search(&u).to_string(),
            _ => url::quirks::hash(&u).to_string(),
        },
    };
    let v = str_val(scope, &s);
    rv.set(v);
}

fn cb_set_href(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !recv(scope, args.this()) {
        return;
    }
    let Some(input) = usv(scope, args.get(0)) else { return };
    let Some(u) = parse_or_throw(scope, &input, "Failed to set the 'href' property on 'Location'") else { return };
    navigate(scope, &u, false);
}

/// The component setters, HTML's steps over the WHATWG URL setters (`url::quirks`): copy the URL, run
/// the component's state-override parse, and navigate to the copy.
fn cb_set_part(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !recv(scope, args.this()) {
        return;
    }
    let part = webapi::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let Some(value) = usv(scope, args.get(0)) else { return };
    let Some(cur) = current_url(scope) else { return };
    let mut copy = cur.clone();
    match part.as_str() {
        "protocol" => {
            // A syntactically invalid scheme is Chrome's SyntaxError; a valid one the URL refuses (a
            // special scheme to a non-special one: `data`) changes nothing; and only http(s) results
            // navigate.
            let scheme = value.split(':').next().unwrap_or("");
            let valid = scheme.chars().next().map(|c| c.is_ascii_alphabetic()).unwrap_or(false)
                && scheme.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.');
            if !valid {
                throw_dom(scope, &format!("Failed to set the 'protocol' property on 'Location': '{value}' is an invalid protocol."), "SyntaxError");
                return;
            }
            if url::quirks::set_protocol(&mut copy, &value).is_err() {
                return;
            }
            if copy.scheme() != "http" && copy.scheme() != "https" {
                return;
            }
        }
        "host" | "hostname" | "port" | "pathname" => {
            if copy.cannot_be_a_base() {
                return;
            }
            let _ = match part.as_str() {
                "host" => url::quirks::set_host(&mut copy, &value),
                "hostname" => url::quirks::set_hostname(&mut copy, &value),
                "port" => url::quirks::set_port(&mut copy, &value),
                _ => {
                    url::quirks::set_pathname(&mut copy, &value);
                    Ok(())
                }
            };
        }
        // Chrome keeps an EMPTY query for `location.search = ""` -- it lands on `page.html?`, measured
        // on all four browsers -- where the URL standard's setter would drop the `?`.
        "search" if value.is_empty() => copy.set_query(Some("")),
        "search" => url::quirks::set_search(&mut copy, &value),
        _ => {
            // HTML's hash setter, not the URL one: the fragment becomes the EMPTY STRING and the input
            // (one leading "#" removed) is parsed into it, so `location.hash = ""` navigates to `…#`
            // (measured) where the URL setter would drop the fragment entirely.
            copy.set_fragment(Some(""));
            let input = value.strip_prefix('#').unwrap_or(&value);
            if !input.is_empty() {
                url::quirks::set_hash(&mut copy, input);
            }
            // Setting the fragment the URL already has is not a navigation at all (measured:
            // `location.hash = "#b"` twice fires nothing and adds no entry).
            // A URL with NO fragment counts as the empty one here: `location.hash = ""` on a page with no
            // fragment neither navigates nor fires anything (WPT `location_hash_set_empty_string.html`,
            // `hash-empty-string.html`, which Chrome passes), while `#e` -> `""` lands on `…#`.
            if copy.fragment().unwrap_or("") == cur.fragment().unwrap_or("") {
                return;
            }
        }
    }
    navigate(scope, &copy, false);
}

fn method_args(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, name: &str) -> Option<url::Url> {
    if !recv(scope, args.this()) {
        return None;
    }
    if args.length() < 1 {
        throw_type_error(scope, &format!("Failed to execute '{name}' on 'Location': 1 argument required, but only 0 present."));
        return None;
    }
    let input = usv(scope, args.get(0))?;
    parse_or_throw(scope, &input, &format!("Failed to execute '{name}' on 'Location'"))
}

fn cb_assign(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if let Some(u) = method_args(scope, &args, "assign") {
        navigate(scope, &u, false);
    }
}

fn cb_replace(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if let Some(u) = method_args(scope, &args, "replace") {
        navigate(scope, &u, true);
    }
}

/// `reload()` -- a new load of the current URL, which is what a challenge token interstitial flow depends on.
fn cb_reload(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if !recv(scope, args.this()) {
        return;
    }
    request_reload(scope);
}

/// `toString()` -- the stringifier, `href`.
fn cb_to_string(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !recv(scope, args.this()) {
        return;
    }
    let href = current_href(scope);
    let s = url::Url::parse(&href).map(|u| url::quirks::href(&u).to_string()).unwrap_or(href);
    let v = str_val(scope, &s);
    rv.set(v);
}

fn cb_ancestor_origins(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !recv(scope, args.this()) {
        return;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let held = op_state.borrow().try_borrow::<AncestorOrigins>().map(|a| a.0.clone());
    let v = match held {
        Some(g) => v8::Local::new(scope, &g),
        None => {
            // A top-level document has no ancestors; a remote frame's has its embedder's chain.
            let list = match webapi::page::ancestor_origins_list(scope) {
                Some(l) => l,
                None => webapi::install::live::make_empty_dom_string_list(scope),
            };
            let g = v8::Global::new(scope, list);
            op_state.borrow_mut().put(AncestorOrigins(g));
            list
        }
    };
    rv.set(v);
}

fn accessor(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, name: &str, get: v8::Local<v8::Function>, set: Option<v8::Local<v8::Function>>) {
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let mut d = v8::PropertyDescriptor::new_from_get_set(get.into(), set.map(|s| s.into()).unwrap_or(undef));
    d.set_enumerable(true);
    d.set_configurable(false);
    let Some(k) = v8::String::new(scope, name) else { return };
    obj.define_property(scope, k.into(), &d);
}

fn unforgeable_value(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, key: v8::Local<v8::Name>, value: v8::Local<v8::Value>, enumerable: bool) {
    let mut d = v8::PropertyDescriptor::new_from_value_writable(value, false);
    d.set_enumerable(enumerable);
    d.set_configurable(false);
    obj.define_property(scope, key, &d);
}

/// Put every `Location` member on `loc` as the OWN, non-configurable property `[LegacyUnforgeable]`
/// makes it, in the browsers' own order (`Object.keys(location)`: `ancestorOrigins href origin
/// protocol host hostname port pathname search hash assign reload replace toString`), then the two
/// non-enumerable ones HTML adds: `valueOf` (the SAME function as `Object.prototype.valueOf`) and
/// `@@toPrimitive` (`undefined`).
pub(crate) fn install_location_members<'s>(scope: &mut v8::PinScope<'s, '_>, loc: v8::Local<'s, v8::Object>) {
    let g = v8::Global::new(scope, loc);
    deno_core::JsRuntime::op_state_from(scope).borrow_mut().put(LocationObject(g));

    // Each from its template: a page realm mints one `Location`.
    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
    let ao = template_fn(scope, cb_ancestor_origins, "get ancestorOrigins", 0, None);
    accessor(scope, loc, "ancestorOrigins", ao, None);
    for part in ["href", "origin", "protocol", "host", "hostname", "port", "pathname", "search", "hash"] {
        let get = template_fn(scope, cb_get, &format!("get {part}"), 0, Some(part));
        let set = match part {
            "origin" => None,
            "href" => Some(template_fn(scope, cb_set_href, "set href", 1, Some(part))),
            _ => Some(template_fn(scope, cb_set_part, &format!("set {part}"), 1, Some(part))),
        };
        accessor(scope, loc, part, get, set);
    }
    let assign = template_fn(scope, cb_assign, "assign", 1, None);
    let reload = template_fn(scope, cb_reload, "reload", 0, None);
    let replace = template_fn(scope, cb_replace, "replace", 1, None);
    let to_string = template_fn(scope, cb_to_string, "toString", 0, None);
    for (name, f) in [("assign", assign), ("reload", reload), ("replace", replace), ("toString", to_string)] {
        let Some(k) = v8::String::new(scope, name) else { continue };
        unforgeable_value(scope, loc, k.into(), f.into(), true);
    }
    // `valueOf` IS `Object.prototype.valueOf` and `@@toPrimitive` is `undefined`, both non-enumerable
    // (measured: `location.valueOf === Object.prototype.valueOf` true on all four browsers).
    // The realm's own `Object.prototype.valueOf`.
    let object_value_of = webapi::binding::protos::intrinsic_method(scope, "Object", "valueOf").map(|f| -> v8::Local<v8::Value> { f.into() });
    if let (Some(vo), Some(k)) = (object_value_of, v8::String::new(scope, "valueOf")) {
        unforgeable_value(scope, loc, k.into(), vo, false);
    }
    let tp = v8::Symbol::get_to_primitive(scope);
    unforgeable_value(scope, loc, tp.into(), u, false);
}

/// Move the page's URL without a navigation -- the session history's same-document step: `location` and the
/// document's own URL (Blink `Document::SetURL`; not its cookie URL).
pub(crate) fn move_url(scope: &mut v8::PinScope, url: &str) {
    set_href(scope, url);
    webapi::set_document_url(scope, url);
}
