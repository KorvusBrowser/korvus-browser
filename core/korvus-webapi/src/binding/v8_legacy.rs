//! `binding/v8_legacy.rs` — the two `v8::ObjectTemplate` entry points `document.all` needs and
//! `rusty_v8` 150.4.0 does not wrap.
//!
//! WHY THIS FILE EXISTS. `document.all` is the web platform's one `[[IsHTMLDDA]]` object: `typeof
//! document.all` is `"undefined"`, `Boolean(document.all)` is `false` and `document.all == null` is
//! `true`, while the object itself is a live `HTMLAllCollection` with a `length`, indexed and named
//! access and a legacy call form. **No JS shim can produce that `typeof`** — it is a bit on the
//! object's V8 map, set by `v8::ObjectTemplate::MarkAsUndetectable()`, and V8 checks it in
//! `Object::TypeOf` BEFORE `IsCallable`. The legacy call form (`document.all(0)`) needs the object to
//! be callable, which for a template-minted object is
//! `v8::ObjectTemplate::SetCallAsFunctionHandler()`.
//!
//! MEASURED, not assumed: neither method is exposed by the `v8` crate this workspace pins
//! (`v8 150.4.0`, `Cargo.lock`). `src/template.rs` has no `mark_as_undetectable` /
//! `set_undetectable` and no `set_call_as_function_handler`; `src/binding.cc` declares no
//! `v8__ObjectTemplate__MarkAsUndetectable` and no `v8__ObjectTemplate__SetCallAsFunctionHandler`.
//! Both C++ methods are nonetheless COMPILED INTO the V8 this workspace links — `nm` on
//! `target/release/gn_out/obj/librusty_v8.a` reports both as defined (`T`):
//!
//! ```text
//! T _ZN2v814ObjectTemplate18MarkAsUndetectableEv
//! T _ZN2v814ObjectTemplate24SetCallAsFunctionHandlerEPFvRKNS_20FunctionCallbackInfoINS_5ValueEEEENS_5LocalIS2_EE
//! ```
//!
//! So this file declares those two symbols and nothing else. It is the REAL V8 API, reached by its
//! Itanium-ABI name because the Rust wrapper for it does not exist — not an approximation of the
//! behaviour, which is the thing that would be worse than the `null` `document.all` replaced: a
//! `document.all` that is TRUTHY flips feature detection the other way on sites that work today.
//!
//! WHAT BREAKS IT AND HOW YOU FIND OUT. A `v8` crate bump that changes the C++ signature, or a
//! platform whose C++ ABI is not Itanium (MSVC), makes these names not resolve and the LINK FAILS —
//! loudly, at build time, naming the symbol. That is the canary, and it is why the declarations sit
//! in one file with the mangled names spelled out. The day `rusty_v8` wraps either method, delete the
//! corresponding declaration and call the wrapper: the signatures are the same.
//!
//! ABI. Both are non-virtual C++ member functions, so the `this` pointer is the first argument.
//! `v8::Local<T>` is `#[repr(C)]` over a `NonNull<T>` — one pointer — and C++ passes an empty
//! `Local` as a null pointer, which is what `SetCallAsFunctionHandler`'s defaulted `data` is.
//! `v8::FunctionCallback` is `unsafe extern "C" fn(*const FunctionCallbackInfo)`, which is exactly
//! C++'s `void (*)(const v8::FunctionCallbackInfo<v8::Value>&)`.

use deno_core::v8;

// These are v8 C++ methods the `v8` crate does not wrap; linked by their mangled name. The name differs
// by the C++ compiler that built librusty_v8: Itanium (clang) on Linux/macOS, MSVC on Windows. The C ABI
// (how `this` and args are passed) is the platform's own on both sides, so only the symbol name is cfg'd.
extern "C" {
    /// `void v8::ObjectTemplate::MarkAsUndetectable()`
    #[cfg_attr(not(windows), link_name = "_ZN2v814ObjectTemplate18MarkAsUndetectableEv")]
    #[cfg_attr(windows, link_name = "?MarkAsUndetectable@ObjectTemplate@v8@@QEAAXXZ")]
    fn v8_object_template_mark_as_undetectable(this: *const v8::ObjectTemplate);

    /// `void v8::ObjectTemplate::SetCallAsFunctionHandler(v8::FunctionCallback, v8::Local<v8::Value>)`
    #[cfg_attr(not(windows), link_name = "_ZN2v814ObjectTemplate24SetCallAsFunctionHandlerEPFvRKNS_20FunctionCallbackInfoINS_5ValueEEEENS_5LocalIS2_EE")]
    #[cfg_attr(windows, link_name = "?SetCallAsFunctionHandler@ObjectTemplate@v8@@QEAAXP6AXAEBV?$FunctionCallbackInfo@VValue@v8@@@2@@ZV?$Local@VValue@v8@@@2@@Z")]
    fn v8_object_template_set_call_as_function_handler(
        this: *const v8::ObjectTemplate,
        callback: v8::FunctionCallback,
        data: *const v8::Value,
    );

    /// `void v8::Context::DetachGlobal()`
    #[cfg_attr(not(windows), link_name = "_ZN2v87Context12DetachGlobalEv")]
    #[cfg_attr(windows, link_name = "?DetachGlobal@Context@v8@@QEAAXXZ")]
    fn v8_context_detach_global(this: *const v8::Context);

    /// `void v8::Context::SetErrorMessageForCodeGenerationFromStrings(v8::Local<v8::String>)`
    #[cfg_attr(not(windows), link_name = "_ZN2v87Context43SetErrorMessageForCodeGenerationFromStringsENS_5LocalINS_6StringEEE")]
    #[cfg_attr(windows, link_name = "?SetErrorMessageForCodeGenerationFromStrings@Context@v8@@QEAAXV?$Local@VString@v8@@@2@@Z")]
    fn v8_context_set_error_message_for_code_generation_from_strings(this: *const v8::Context, message: *const v8::String);
}

/// Instances minted from `tmpl` carry V8's UNDETECTABLE bit: `typeof` answers `"undefined"`,
/// `ToBoolean` answers `false`, and `== null` / `== undefined` are `true` — while `=== undefined` is
/// `false` and every property still reads. This is `[[IsHTMLDDA]]`; `document.all` is its only user
/// on the platform.
pub(crate) fn mark_as_undetectable(tmpl: v8::Local<v8::ObjectTemplate>) {
    // SAFETY: `tmpl` is a live `Local<ObjectTemplate>` and the call takes only its `this` pointer.
    unsafe { v8_object_template_mark_as_undetectable(&*tmpl) }
}

/// Instances minted from `tmpl` are CALLABLE: `obj(args)` and `Reflect.apply(obj, …)` enter
/// `callback`, which receives `data` as `args.data()`. The object is NOT a constructor (`new obj()`
/// is a TypeError), which is what `document.all` answers.
pub(crate) fn set_call_as_function_handler(
    tmpl: v8::Local<v8::ObjectTemplate>,
    callback: impl v8::MapFnTo<v8::FunctionCallback>,
    data: v8::Local<v8::Value>,
) {
    // SAFETY: `tmpl` and `data` are live `Local`s; `callback` is mapped to the same `extern "C"`
    // function-pointer type V8 declares for a function callback.
    unsafe { v8_object_template_set_call_as_function_handler(&*tmpl, crate::binding::refs::c(callback.map_fn_to()), &*data) }
}

/// Detach `ctx`'s global object from its global proxy (`v8::Context::DetachGlobal`), before a new realm
/// reuses the proxy. V8 gives the old global a proxy of its own (`global_proxy_for_api`) as the holder of
/// its old code's accessor reads, so a lazy data property the old global still has is replaced on the OLD
/// global rather than through the reused proxy on the new one (`core/html/browsing_context.rs`,
/// `forget_previous_window`). Also not wrapped by the `v8` crate this workspace pins; `nm` on
/// `librusty_v8.a` reports `T _ZN2v87Context12DetachGlobalEv`.
pub(crate) fn detach_global(ctx: v8::Local<v8::Context>) {
    // SAFETY: `ctx` is a live `Local<Context>`; the call takes only its `this` pointer.
    unsafe { v8_context_detach_global(&*ctx) }
}

// `v8::FunctionTemplate::HasInstance` was linked here for the first step and is gone again: it
// follows `FunctionTemplate::Inherit`, which 214 templates cannot have (built before their parent's), so the
// brand a check asks for is the template-carried one in `binding/brand.rs`.

/// The message of the EvalError V8 throws in `ctx` when code generation from a string is refused
/// (`v8::Context::SetErrorMessageForCodeGenerationFromStrings`): Blink gives each context its CSP's
/// sentence (`ContentSecurityPolicy::EvalDisabledErrorMessage`), and V8's default ("Code generation from
/// strings disallowed for this context") is what a page saw here. Not wrapped by the `v8` crate this
/// workspace pins; `nm` on `librusty_v8.a` reports
/// `T _ZN2v87Context43SetErrorMessageForCodeGenerationFromStringsENS_5LocalINS_6StringEEE`.
pub(crate) fn set_error_message_for_code_generation_from_strings(ctx: v8::Local<v8::Context>, message: v8::Local<v8::String>) {
    // SAFETY: `ctx` and `message` are live `Local`s; a `Local` is passed as its one handle pointer.
    unsafe { v8_context_set_error_message_for_code_generation_from_strings(&*ctx, &*message) }
}
