//! `core/html/referrer_policy.rs` -- a document's referrer policy, and the policy each of its requests
//! goes out under (Referrer Policy sec. 3-8; HTML "the meta element", `name=referrer`).
//!
//! A document's policy is the one its RESPONSE delivered (`Referrer-Policy`, the last recognised token
//! winning), replaced by every `<meta name=referrer>` the document holds -- the last one in tree order.
//! An element's own `referrerpolicy` and a `fetch` init's `referrerPolicy` override it for that one
//! request. An `about:blank`/`about:srcdoc` frame inherits its creator's policy, as it inherits its
//! creator's client.
//!
//! MEASURED, stock Chrome 150 headed, `tools/realweb/seed/refperm` (a page on :A served with
//! `Referrer-Policy: same-origin`, requests to :B):
//!
//! ```text
//! <img>, <script src>, fetch(no-cors), <iframe> to :B         no Referer;  frame document.referrer ""
//! <img referrerpolicy=unsafe-url>, fetch {referrerPolicy}      Referer http://A/p.html (the full URL)
//! same-origin fetch / <iframe src=/f.html>                     Referer http://A/p.html
//! <meta name=referrer content=no-referrer> (no header)         no Referer anywhere, frame referrer ""
//! ```
//!
//! Before this file every request went out under Chrome's DEFAULT policy whatever the document said, so
//! a bot challenge page -- which is served `Referrer-Policy: same-origin` -- had korvus send
//! `Referer: https://<site>/` on every request to `a challenge host` where Chrome sends none.

use deno_core::OpState;
use transport::ReferrerPolicy;

use crate::binding::HostDom;

/// The page document's DELIVERED policy: its response's `Referrer-Policy`. Replaced at every
/// navigation ([`set_document_referrer_policy`]); a frame document's lives on its browsing context.
#[derive(Default, Clone, Copy)]
pub(crate) struct DocReferrerPolicy(pub Option<ReferrerPolicy>);

/// The page document's delivered policy, from the response headers the navigation delivered.
pub fn set_document_referrer_policy(state: &mut OpState, headers: &[(String, String)]) {
    state.put(DocReferrerPolicy(ReferrerPolicy::from_headers(headers)));
}

/// `<meta name=referrer content>`'s value (HTML 4.2.5.3): a policy token, or one of the four legacy
/// keywords Chrome still maps.
fn meta_token(content: &str) -> Option<ReferrerPolicy> {
    match content.trim().to_ascii_lowercase().as_str() {
        "never" => Some(ReferrerPolicy::NoReferrer),
        "default" => Some(ReferrerPolicy::StrictOriginWhenCrossOrigin),
        "always" => Some(ReferrerPolicy::UnsafeUrl),
        "origin-when-crossorigin" => Some(ReferrerPolicy::OriginWhenCrossOrigin),
        other => ReferrerPolicy::from_token(other),
    }
}

/// The last `<meta name=referrer>` of the document at `root`, in tree order, that names a policy.
pub(crate) fn meta_policy(dom: &korvus_dom::Dom, root: u32) -> Option<ReferrerPolicy> {
    dom.get_elements_by_tag_name_in(root, "meta")
        .into_iter()
        .filter(|m| dom.get_attribute(*m, "name").is_some_and(|n| n.trim().eq_ignore_ascii_case("referrer")))
        .filter_map(|m| dom.get_attribute(m, "content").and_then(meta_token))
        .last()
}

/// The PAGE document's policy, from the realm state alone -- for the runtime's own request builders
/// (the parser's script pass, module fetches), which hold an `OpState` and no scope.
pub fn page_document_policy(state: &OpState) -> Option<ReferrerPolicy> {
    let meta = state.try_borrow::<HostDom>().and_then(|h| meta_policy(&h.0, h.0.root()));
    meta.or_else(|| state.try_borrow::<DocReferrerPolicy>().and_then(|p| p.0))
}

/// An element's own `referrerpolicy`, when it names a policy (an empty or unknown value sets none).
pub(crate) fn element_policy(dom: &korvus_dom::Dom, el: u32) -> Option<ReferrerPolicy> {
    dom.get_attribute(el, "referrerpolicy").and_then(ReferrerPolicy::from_token)
}

/// The policy of the document of browsing context `ctx` (`None` = the page's).
pub(crate) fn context_policy(scope: &mut deno_core::v8::PinScope, ctx: Option<u32>) -> Option<ReferrerPolicy> {
    let Some(id) = ctx else {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        return page_document_policy(&st);
    };
    crate::core::html::browsing_context::frame_document_referrer_policy(scope, id)
}

/// The policy the RUNNING realm's requests go out under: the running frame's document's, else the page's.
pub(crate) fn running_policy(scope: &mut deno_core::v8::PinScope) -> Option<ReferrerPolicy> {
    let ctx = crate::core::html::browsing_context::running_frame(scope);
    context_policy(scope, ctx)
}

/// The policy an ELEMENT's load goes out under: its own `referrerpolicy`, else its document's.
pub(crate) fn element_request_policy(scope: &mut deno_core::v8::PinScope, el: u32) -> Option<ReferrerPolicy> {
    let own = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<HostDom>().and_then(|h| element_policy(&h.0, el))
    };
    if own.is_some() {
        return own;
    }
    match crate::core::html::subresource::home_of(scope, el) {
        crate::core::html::subresource::Home::Frame(ctx) => context_policy(scope, Some(ctx)),
        // A detached element (`new Image()`) loads under its node document's -- the running realm's.
        crate::core::html::subresource::Home::Nowhere => running_policy(scope),
        crate::core::html::subresource::Home::Page => context_policy(scope, None),
    }
}

/// The token a request record carries to the runtime (`""` = the default policy).
pub(crate) fn token(p: Option<ReferrerPolicy>) -> &'static str {
    p.map(ReferrerPolicy::token).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_content_maps_the_legacy_keywords_and_rejects_garbage() {
        assert_eq!(meta_token("never"), Some(ReferrerPolicy::NoReferrer));
        assert_eq!(meta_token(" Always "), Some(ReferrerPolicy::UnsafeUrl));
        assert_eq!(meta_token("origin-when-crossorigin"), Some(ReferrerPolicy::OriginWhenCrossOrigin));
        assert_eq!(meta_token("same-origin"), Some(ReferrerPolicy::SameOrigin));
        assert_eq!(meta_token("bogus"), None);
        assert_eq!(meta_token(""), None);
    }
}
