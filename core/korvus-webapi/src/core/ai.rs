//! `core/ai.rs` -- the built-in AI APIs' static entry points: `Summarizer`, `LanguageModel`, `LanguageDetector` and
//! `Translator` `.availability()` and `.create()`.
//!
//! WHAT WAS HERE BEFORE. Registry stubs whose promises resolved `undefined`. Stock Chrome 150 resolves
//! `availability()` with an `Availability` value before the call returns, and a bot-challenge frame reads
//! `Summarizer.availability()`. WHAT it answers is the persona's (`spec::ClientHints::ai_availability`, measured on
//! the capture machine: `"unavailable"` for all four under Chrome 150 here). A value the bundle marks `pending`
//! (Edge's and Opera's that never settled) is a promise that never settles.
//!
//! THE OPTIONS convert first, as Blink's bindings convert a dictionary: its members in lexicographic order, the
//! inherited dictionary's before the derived one's, each enum checked against its values and each `required` member
//! present. An error REJECTS the promise -- a static operation returning a promise never throws (Chrome 150:
//! `Summarizer.availability({type: "nope"})` rejects with Blink's `TypeError`, `Translator.availability()` with its
//! arity message).
//!
//! `create()` rejects as Chrome 150 does when nothing is available to create: in a frame whose permissions policy
//! does not allow the feature (`summarizer`, `language-model`, ... default `self`: a cross-origin frame without
//! `allow=`) with "Access denied because the Permission Policy is not enabled.", elsewhere -- without a user gesture,
//! the case measured -- with the user-gesture `NotAllowedError`.
//!
//! `LanguageModelCreateCoreOptions`' members are nested dictionaries of prompts and tools; they are read (each
//! getter runs) but only its numeric members are converted here.

use deno_core::v8;

use crate::binding::protos::{make_dom_exc, rejected_promise, resolved_promise, str_val};
use crate::install::MemberDef;

/// The body every `availability` / `create` static row names.
pub fn install_ai(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let iface = member.label.split("::").next().unwrap_or("");
    let d = str_val(scope, &format!("{iface}.{}", member.name));
    crate::core::dom::shared::row_method_data(scope, proto, member, cb_ai, d);
}

/// One dictionary member: its name and how it converts.
enum Kind {
    Enum(&'static str, &'static [&'static str]),
    Str,
    RequiredStr,
    Strings,
    Callback,
    Signal,
    Number,
    Any,
}

/// The members of `iface`'s options dictionary for `method`, in conversion order, and the dictionary names
/// (core, derived) the messages use.
fn members(iface: &str, method: &str) -> (Vec<(&'static str, Kind)>, &'static str, &'static str) {
    const SUM_FORMAT: &[&str] = &["plain-text", "markdown"];
    const SUM_LENGTH: &[&str] = &["short", "medium", "long"];
    const SUM_TYPE: &[&str] = &["tldr", "key-points", "teaser", "headline"];
    const PREFERENCE: &[&str] = &["auto", "speed", "capability"];
    let (mut core, core_name, derived_name): (Vec<(&'static str, Kind)>, &str, &str) = match iface {
        "Summarizer" => (
            vec![
                ("expectedContextLanguages", Kind::Strings),
                ("expectedInputLanguages", Kind::Strings),
                ("format", Kind::Enum("SummarizerFormat", SUM_FORMAT)),
                ("length", Kind::Enum("SummarizerLength", SUM_LENGTH)),
                ("outputLanguage", Kind::Str),
                ("preference", Kind::Enum("PerformancePreference", PREFERENCE)),
                ("type", Kind::Enum("SummarizerType", SUM_TYPE)),
            ],
            "SummarizerCreateCoreOptions",
            "SummarizerCreateOptions",
        ),
        "LanguageDetector" => (vec![("expectedInputLanguages", Kind::Strings)], "LanguageDetectorCreateCoreOptions", "LanguageDetectorCreateOptions"),
        "Translator" => (
            vec![("sourceLanguage", Kind::RequiredStr), ("targetLanguage", Kind::RequiredStr)],
            "TranslatorCreateCoreOptions",
            "TranslatorCreateOptions",
        ),
        _ => (
            vec![
                ("expectedInputs", Kind::Any),
                ("expectedOutputs", Kind::Any),
                ("temperature", Kind::Number),
                ("tools", Kind::Any),
                ("topK", Kind::Number),
            ],
            "LanguageModelCreateCoreOptions",
            "LanguageModelCreateOptions",
        ),
    };
    if method == "create" {
        core.push(("monitor", Kind::Callback));
        if iface == "Summarizer" {
            core.push(("sharedContext", Kind::Str));
        }
        if iface == "LanguageModel" {
            core.push(("initialPrompts", Kind::Any));
        }
        core.push(("signal", Kind::Signal));
    }
    (core, core_name, derived_name)
}

/// Convert `v` as `iface`'s options dictionary for `method`. `None` with an exception pending.
fn convert(scope: &mut v8::PinScope, iface: &str, method: &str, v: v8::Local<v8::Value>) -> Option<()> {
    let (members, core_name, derived_name) = members(iface, method);
    let dict = if method == "create" { derived_name } else { core_name };
    let at = format!("Failed to execute '{method}' on '{iface}': ");
    if v.is_null_or_undefined() {
        // an absent dictionary is {}: only its required members can fail
        if let Some((key, _)) = members.iter().find(|(_, k)| matches!(k, Kind::RequiredStr)) {
            crate::binding::protos::throw_type_error(scope, &format!("{at}Failed to read the '{key}' property from '{dict}': Required member is undefined."));
            return None;
        }
        return Some(());
    }
    let Ok(obj) = v8::Local::<v8::Object>::try_from(v) else {
        crate::binding::protos::throw_type_error(scope, &format!("{at}The provided value is not of type '{dict}'."));
        return None;
    };
    for (key, kind) in members {
        // the member's own dictionary: the core one's, or the derived one's for create()'s own members
        let owner = if matches!(key, "monitor" | "signal" | "sharedContext" | "initialPrompts") { derived_name } else { core_name };
        let read = format!("{at}Failed to read the '{key}' property from '{owner}': ");
        let k = str_val(scope, key);
        let x = obj.get(scope, k)?;
        if x.is_undefined() {
            if matches!(kind, Kind::RequiredStr) {
                crate::binding::protos::throw_type_error(scope, &format!("{read}Required member is undefined."));
                return None;
            }
            continue;
        }
        match kind {
            Kind::Enum(ty, values) => {
                let s = x.to_string(scope)?.to_rust_string_lossy(scope);
                if !values.contains(&s.as_str()) {
                    crate::binding::protos::throw_type_error(scope, &format!("{read}The provided value '{s}' is not a valid enum value of type {ty}."));
                    return None;
                }
            }
            Kind::Str | Kind::RequiredStr => {
                x.to_string(scope)?;
            }
            Kind::Strings => {
                crate::core::timing::performance_observer::string_sequence(scope, x, &read)?;
            }
            Kind::Callback => {
                if !x.is_function() {
                    crate::binding::protos::throw_type_error(scope, &format!("{read}The provided value is not a function."));
                    return None;
                }
            }
            Kind::Signal => {
                if !crate::binding::protos::implements(scope, x, "AbortSignal") {
                    crate::binding::protos::throw_type_error(scope, &format!("{read}Failed to convert value to 'AbortSignal'."));
                    return None;
                }
            }
            Kind::Number => {
                x.number_value(scope)?;
            }
            Kind::Any => {}
        }
    }
    Some(())
}

/// The permissions-policy feature an interface's `create()` needs.
fn feature(iface: &str) -> &'static str {
    match iface {
        "Summarizer" => "summarizer",
        "LanguageDetector" => "language-detector",
        "Translator" => "translator",
        _ => "language-model",
    }
}

fn cb_ai(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let d = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let (iface, method) = d.split_once('.').unwrap_or(("", d.as_str()));
    // the options convert first; a conversion error is the promise's rejection
    let converted = {
        v8::tc_scope!(let tc, scope);
        let ok = convert(tc, iface, method, args.get(0)).is_some();
        let exc = if tc.has_caught() { tc.exception().map(|e| v8::Global::new(tc, e)) } else { None };
        tc.reset();
        (ok, exc)
    };
    if let (false, Some(e)) = converted {
        let e = v8::Local::new(scope, &e);
        let p = rejected_promise(scope, e);
        rv.set(p);
        return;
    }
    if method == "create" {
        let msg = if crate::core::feature_policy::enabled_in_current_realm(scope, feature(iface)) {
            "Requires a user gesture when availability is \"downloading\" or \"downloadable\"."
        } else {
            "Access denied because the Permission Policy is not enabled."
        };
        let Some(e) = make_dom_exc(scope, msg, "NotAllowedError") else { return };
        let p = rejected_promise(scope, e);
        rv.set(p);
        return;
    }
    // availability(): the persona's answer
    let answer = crate::core::navigator::navigator::client_hints(scope)
        .ai_availability
        .iter()
        .find(|(k, _)| k == iface)
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| "unavailable".to_string());
    if answer == "pending" {
        if let Some(r) = v8::PromiseResolver::new(scope) {
            let p = r.get_promise(scope);
            rv.set(p.into());
        }
        return;
    }
    if let Some(rest) = answer.strip_prefix("reject:") {
        let (name, msg) = rest.split_once(':').unwrap_or((rest, ""));
        let Some(e) = make_dom_exc(scope, msg, name) else { return };
        let p = rejected_promise(scope, e);
        rv.set(p);
        return;
    }
    let v = str_val(scope, &answer);
    let p = resolved_promise(scope, v);
    rv.set(p);
}
