//! LIVE gate: `error.stack`, a page's `Error.prepareStackTrace` and its call sites, `Error.captureStackTrace`,
//! `Error.stackTraceLimit`, and the errors the platform throws, as V8 + Blink make them (ledger N13).
//!
//! THE STACK IS V8'S OWN. Blink sets no prepare-stack-trace callback; deno_core set one on every isolate
//! (`error::prepare_stack_trace_callback`) and formatted every `stack` itself. A page could read it: inside an ES
//! module carrying an inline `//# sourceMappingURL` every frame read `:1:1` (deno_core registered and applied the
//! page's source map -- Fingerprint's s119 collects exactly that frame), a page's `Error.prepareStackTrace` got
//! deno's patched call sites (18 methods, no `getPosition` / `getEnclosingLineNumber` / `getEnclosingColumnNumber` /
//! `getScriptHash`, no `CallSite` constructor, "The receiver is not a valid callsite object." for V8's "CallSite
//! method getFileName expects CallSite as receiver") looked up on the current realm's global `Error` rather than the
//! error's own realm's intrinsic, an eval frame named by `//# sourceURL` read `zz.js, <anonymous>:1:1`, a module's
//! own `//# sourceURL` was ignored, the head line took the CURRENT `message` (V8 takes the one the error was made
//! with) and a throwing `name` read `Error` where V8 prints `<error: TypeError: ...>`. With no callback, V8 formats
//! (`vendor/deno_core/runtime/setup.rs`), and page modules' source maps are not registered at all
//! (`vendor/deno_core/modules/map.rs`).
//!
//! A PLATFORM ERROR'S CONTEXT COMES AFTER IT IS MADE. Blink adds "Failed to execute 'm' on 'T': " in V8's
//! exception-propagation callback (`ApplyContextToException`): an ECMAScript error's `message` is redefined with
//! `CreateDataProperty` -- enumerable -- while V8 formats `stack` from the message the error was made with
//! (`--use-original-message-for-stack-trace`). So Chrome's `document.body.appendChild(5)` has `message` "Failed to
//! execute 'appendChild' on 'Node': parameter 1 is not of type 'Node'.", `stack` "TypeError: parameter 1 is not of
//! type 'Node'.\n    at ...", and `Object.keys(e)` `["message"]`; this engine composed the context into the message
//! it made the error with. `deno_core::error::native_error` makes such an error as Blink's is made, and every
//! helper that throws or rejects with one goes through it. A DOMException takes the context into its own message,
//! which its `stack` reads -- unchanged, and Chrome's.
//!
//! **Expectations read from Google Chrome 150.0.7871.46 on this machine on 2026-10-11** (headed under Xvfb, no
//! DevTools attached, three runs alike): `fixtures/stack-format/probe.js` answered `fixtures/stack-format/
//! chrome-150.json` and `fixtures/stack-format/native-errors-probe.js` answered `fixtures/stack-format/
//! native-errors-chrome-150.json`, each inlined in `fixtures/canvas-and-errors/probe-page.html` at 127.0.0.1 as
//! this file serves it. Blob URLs read `blob:URL`, ports `PORT`, and a frame's `:line:col` reads `#line#col` (the
//! probe's own spelling, so the harness's port normalisation leaves the numbers alone).
//!
//! Left out of the platform-error rows, each a different unit's difference (measured on the same day): `new
//! TextDecoder('nope')` (this engine's message has no context at all), `new Image().width = Symbol()` (converted as
//! a string, Chrome says "...to a number"), `new Headers([['a']])` and `Headers.append('a b', 'c')` (not refused
//! here) and `new ReadableStream({type: 'x'})` (not refused here).

use crate::chrome_canvas_and_errors_live::{answers, assert_as_chrome, Mode};

/// Stacks, call sites, `captureStackTrace` and `stackTraceLimit`, inside blob modules (with and without an inline
/// source map, with a `//# sourceURL`), eval frames, async frames, the realm whose `prepareStackTrace` is asked.
#[tokio::test]
async fn error_stacks_and_call_sites_are_v8s_own() {
    let got = answers(Mode::Page, include_str!("fixtures/stack-format/probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/stack-format/chrome-152.json"));
}

/// What the platform's errors read -- `message`, the stack's head line, own and enumerable properties -- thrown
/// and rejected, ECMAScript errors and DOMExceptions.
#[tokio::test]
async fn a_platform_errors_context_is_applied_after_it_is_made() {
    let got = answers(Mode::Page, include_str!("fixtures/stack-format/native-errors-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/stack-format/native-errors-chrome-152.json"));
}

/// Every context form `ExceptionMessages::AddContextToMessage` writes (platform/bindings/exception_messages.cc), and
/// what is left once all of them are off -- the message the error is made with. A message with no context is left
/// alone, and so is one that merely starts with "Failed to".
#[test]
fn blinks_context_grammar() {
    use deno_core::error::exception_context_detail as detail;
    for (whole, made) in [
        ("Failed to execute 'appendChild' on 'Node': parameter 1 is not of type 'Node'.", Some("parameter 1 is not of type 'Node'.")),
        ("Failed to construct 'URL': Invalid URL", Some("Invalid URL")),
        ("Failed to construct 'IntersectionObserver'", Some("")),
        ("Failed to execute 'open' on 'XMLHttpRequest'", Some("")),
        ("Failed to read the 'width' property from 'Foo': x", Some("x")),
        ("Failed to set the 'width' property on 'HTMLImageElement': Cannot convert a Symbol value to a number", Some("Cannot convert a Symbol value to a number")),
        ("Failed to enumerate the properties of 'Storage': x", Some("x")),
        ("Failed to read an indexed property [3] from 'HTMLCollection': x", Some("x")),
        ("Failed to set an indexed property on 'HTMLSelectElement': x", Some("x")),
        ("Failed to delete an indexed property [0] from 'Foo': x", Some("x")),
        ("Failed to read a named property 'k' from 'Window': x", Some("x")),
        ("Failed to set a named property on 'Storage': x", Some("x")),
        ("Failed to delete a named property 'k' from 'Storage': x", Some("x")),
        (
            "Failed to execute 'query' on 'Permissions': Failed to read the 'name' property from 'PermissionDescriptor': The provided value 'nope' is not a valid enum value of type PermissionName.",
            Some("The provided value 'nope' is not a valid enum value of type PermissionName."),
        ),
        ("Failed to fetch", None),
        ("Failed to fetch dynamically imported module: http://a/b.js", None),
        ("Failed to execute 'initCustomEvent': 1 argument required", None),
        ("Illegal invocation", None),
        ("parameter 1 is not of type 'Node'.", None),
    ] {
        assert_eq!(detail(whole), made, "{whole}");
    }
}
