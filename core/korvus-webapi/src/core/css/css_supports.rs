//! `CSS.supports()` — answered from the style engine's own property table, not from a constant.
//!
//! WHY THIS FILE EXISTS
//!
//! `CSS.supports` used to be a regex over the SHAPE of its argument:
//!
//! ```js
//! supports(prop, value) { // No layout/style engine; accept well-formed input conservatively.
//!   if (value === undefined) return /^[a-z-]+\s*:\s*.+$/i.test(String(prop));
//!   return typeof prop === "string" && prop.length > 0 && value != null && String(value).length > 0;
//! }
//! ```
//!
//! Every syntactically well-formed declaration answered `true`, including `korvus-not-a-prop: 1`.
//! `CSS.supports` is one of the most-called capability queries on the web and its entire contract is
//! to say NO to the things this UA cannot do; a UA that says yes to everything is both wrong and a
//! fingerprint. Track "axis 7" (`tests/constant_answers.rs`) flagged it as a constant answer.
//!
//! The comment was also out of date: there IS a style engine, and it has named every property it
//! understands since long before this. `page::style::property::PropertyId::from_name` is the table --
//! 478 longhands and shorthands -- and `shorthand::expand` knows which shorthands accept which values.
//!
//! WHAT IS AND IS NOT VALIDATED, measured against Brave 151 on this machine:
//!
//! | input                       | Brave | here  |
//! |-----------------------------|-------|-------|
//! | `color: red`                | true  | true  |
//! | `display: grid`             | true  | true  |
//! | `korvus-not-a-prop: 1`      | false | false |
//! | `width: 10px`               | true  | true  |
//! | `(color: red)`              | true  | true  |
//! | `color:red;`                | false | false |
//! | `CSS.supports()`            | throws| throws|
//! | `color: not-a-color`        | false | false |
//! | `width: 10zz`               | false | false |
//!
//! The last two were TRUE here: this file answered from the property NAME alone,
//! while the style engine -- which `@supports` already asked -- held values to its own checks. Both
//! forms now ask `page::style::supports`, so `CSS.supports` and `@supports` cannot disagree, and the
//! value grammars that file knows (Blink's keyword lists, shorthand expansion, numbers, integers,
//! transform functions, non-negative lengths) answer here too; `tools/realweb/seed/csssupports` is
//! the Chrome 150 measurement. A property whose value grammar neither file knows still answers from
//! the cascade's acceptance.

use deno_core::v8;

use crate::binding::protos::str_val;
use crate::install::live::method_row;
use crate::install::MemberDef;

/// `CSS.supports(property, value)` — the two-argument form: the style engine's answer for the
/// value (an empty value is `false` there, as in Chrome: `CSS.supports("color", "")`), which takes no
/// `!important` (`page::style::supports::supports_value`).
fn supports_pair(prop: &str, value: &str) -> bool {
    page::style::supports::supports_value(prop.trim(), value)
}

/// `CSS.supports(conditionText)` — the one-argument form. CSSOM: true when the text is a
/// `<supports-condition>` that holds, or when it does wrapped in parentheses (so `color: red` and
/// `(color: red)` both work, and `selector(:link)` too -- every selector once failed here,
/// the text having been split at its first `:`). A trailing `;` makes it invalid in Chrome
/// (`color:red;` -> false): the argument is one declaration, not a block.
fn supports_condition(cond: &str) -> bool {
    let s = cond.trim();
    if s.trim_end_matches(')').trim_end().ends_with(';') {
        return false;
    }
    page::style::supports::evaluate(s) || page::style::supports::evaluate(&format!("({s})"))
}


// ── the `CSS` namespace ─────────────────────────────────────────────────────────────────────────
//
// `js/dom/06-misc.js` (61 lines) built `globalThis.CSS` as an OBJECT LITERAL with two JS method
// bodies. Three things were wrong with that and all three are page-visible: `String(CSS.escape)`
// printed JS source rather than `[native code]`, `Object.prototype.toString.call(CSS)` answered
// `[object Object]` where Chrome 151 answers `[object CSS]`, and `window.CSS` was enumerable where
// Chrome's is not. The rows below are the table; `install_window_css` mounts them.
//
// A WebIDL NAMESPACE, not an interface: there is no `CSS` constructor and no `CSS.prototype` —
// `escape` and `supports` are OWN properties of the namespace object and `Object.getPrototypeOf(CSS)`
// is `Object.prototype`. So the rows are `static_*` (the installer's word for "on the interface
// object, not on a prototype") and the install mounts them onto a bare object through
// `installer::install_members`, THE member mount, rather than through `create_interface`, which would
// build a constructor the namespace must not have.

/// `CSS.escape(value)` — CSSOM's "serialize an identifier".
///
/// Ported line for line from the deleted JS. It reads `value` as a sequence of characters rather than
/// of UTF-16 code units, which differs only for a lone surrogate (V8's lossy conversion makes it
/// U+FFFD, and both that and the surrogate take the same `>= 0x80` branch) and for an astral first
/// character, whose two units both take that branch too — the `i == 0` / `i == 1` rules can only fire
/// for ASCII digits and `-`.
fn cb_css_escape(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = args.get(0).to_string(scope) else { return };
    let s = s.to_rust_string_lossy(scope);
    let n = s.chars().count();
    let mut out = String::with_capacity(s.len());
    let first = s.chars().next().unwrap_or('\0');
    for (i, ch) in s.chars().enumerate() {
        let c = ch as u32;
        // NULL becomes the replacement character.
        if c == 0 {
            out.push('\u{FFFD}');
            continue;
        }
        // A control character, or a leading digit (or a digit after a leading `-`), is escaped as a
        // hex code point plus one space.
        if (0x1..=0x1f).contains(&c)
            || c == 0x7f
            || (i == 0 && (0x30..=0x39).contains(&c))
            || (i == 1 && (0x30..=0x39).contains(&c) && first == '-')
        {
            out.push('\\');
            out.push_str(&format!("{c:x}"));
            out.push(' ');
            continue;
        }
        // A lone `-` is escaped; `-` elsewhere is not.
        if i == 0 && c == 0x2d && n == 1 {
            out.push_str("\\-");
            continue;
        }
        if c >= 0x80
            || c == 0x2d
            || c == 0x5f
            || (0x30..=0x39).contains(&c)
            || (0x41..=0x5a).contains(&c)
            || (0x61..=0x7a).contains(&c)
        {
            out.push(ch);
            continue;
        }
        out.push('\\');
        out.push(ch);
    }
    let v = str_val(scope, &out);
    rv.set(v);
}

/// `CSS.supports(conditionText)` / `CSS.supports(property, value)`.
///
/// The row is `len 1 required 1`, so the installer's WebIDL arity guard raises Chrome's
/// `Failed to execute 'supports' on 'CSS': 1 argument required, but only 0 present.` before this runs
/// — the exact throw the deleted JS body wrote out by hand. Which overload applies is
/// `arguments.length`, which is why this is a raw callback and not an `#[op2]` pair.
fn cb_css_supports(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(a) = args.get(0).to_string(scope) else { return };
    let a = a.to_rust_string_lossy(scope);
    let answer = if args.length() < 2 {
        supports_condition(&a)
    } else {
        let Some(b) = args.get(1).to_string(scope) else { return };
        supports_pair(&a, &b.to_rust_string_lossy(scope))
    };
    rv.set_bool(answer);
}

pub fn install_css(scope: &mut v8::PinScope, target: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::core::css::typed_om;
    match member.name {
        "escape" => method_row(scope, target, member, cb_css_escape),
        "supports" => method_row(scope, target, member, cb_css_supports),
        "registerProperty" => method_row(scope, target, member, typed_om::cb_css_register_property),
        unit if typed_om::FACTORIES.contains(&unit) => {
            let d = str_val(scope, unit);
            let f = crate::binding::protos::build_fn_data(scope, typed_om::cb_css_unit, unit, 1, d);
            crate::install::kits::define_value(scope, target, member, f.into(), true);
        }
        other => debug_assert!(false, "install_css: unexpected member {other}"),
    }
}

webapi::interface! {
    // Chrome 151, measured: `escape` and `supports` are own, enumerable, configurable, writable
    // methods of length 1, and the namespace object's `Symbol.toStringTag` is "CSS". Chrome 150
    // (measured): the Typed OM factories (`css_unit_values.idl`) and
    // `registerProperty` are methods of the same shape, and `Object.getOwnPropertyNames(CSS)` lists the
    // methods in THIS order -- `highlights` before them and `paintWorklet` after (`install::live::css`).
    "CSS" [singleton, window] {
        static_method "Hz" : Any len 1 required 1 = install_css,
        static_method "Q" : Any len 1 required 1 = install_css,
        static_method "cap" : Any len 1 required 1 = install_css,
        static_method "ch" : Any len 1 required 1 = install_css,
        static_method "cm" : Any len 1 required 1 = install_css,
        static_method "cqb" : Any len 1 required 1 = install_css,
        static_method "cqh" : Any len 1 required 1 = install_css,
        static_method "cqi" : Any len 1 required 1 = install_css,
        static_method "cqmax" : Any len 1 required 1 = install_css,
        static_method "cqmin" : Any len 1 required 1 = install_css,
        static_method "cqw" : Any len 1 required 1 = install_css,
        static_method "deg" : Any len 1 required 1 = install_css,
        static_method "dpcm" : Any len 1 required 1 = install_css,
        static_method "dpi" : Any len 1 required 1 = install_css,
        static_method "dppx" : Any len 1 required 1 = install_css,
        static_method "dvb" : Any len 1 required 1 = install_css,
        static_method "dvh" : Any len 1 required 1 = install_css,
        static_method "dvi" : Any len 1 required 1 = install_css,
        static_method "dvmax" : Any len 1 required 1 = install_css,
        static_method "dvmin" : Any len 1 required 1 = install_css,
        static_method "dvw" : Any len 1 required 1 = install_css,
        static_method "em" : Any len 1 required 1 = install_css,
        static_method "escape"   : DomString len 1 required 1 = install_css,
        static_method "ex" : Any len 1 required 1 = install_css,
        static_method "fr" : Any len 1 required 1 = install_css,
        static_method "grad" : Any len 1 required 1 = install_css,
        static_method "ic" : Any len 1 required 1 = install_css,
        static_method "in" : Any len 1 required 1 = install_css,
        static_method "kHz" : Any len 1 required 1 = install_css,
        static_method "lh" : Any len 1 required 1 = install_css,
        static_method "lvb" : Any len 1 required 1 = install_css,
        static_method "lvh" : Any len 1 required 1 = install_css,
        static_method "lvi" : Any len 1 required 1 = install_css,
        static_method "lvmax" : Any len 1 required 1 = install_css,
        static_method "lvmin" : Any len 1 required 1 = install_css,
        static_method "lvw" : Any len 1 required 1 = install_css,
        static_method "mm" : Any len 1 required 1 = install_css,
        static_method "ms" : Any len 1 required 1 = install_css,
        static_method "number" : Any len 1 required 1 = install_css,
        static_method "pc" : Any len 1 required 1 = install_css,
        static_method "percent" : Any len 1 required 1 = install_css,
        static_method "pt" : Any len 1 required 1 = install_css,
        static_method "px" : Any len 1 required 1 = install_css,
        static_method "rad" : Any len 1 required 1 = install_css,
        static_method "rcap" : Any len 1 required 1 = install_css,
        static_method "rch" : Any len 1 required 1 = install_css,
        static_method "registerProperty" : Undefined len 1 required 1 = install_css,
        static_method "rem" : Any len 1 required 1 = install_css,
        static_method "rex" : Any len 1 required 1 = install_css,
        static_method "ric" : Any len 1 required 1 = install_css,
        static_method "rlh" : Any len 1 required 1 = install_css,
        static_method "s" : Any len 1 required 1 = install_css,
        static_method "supports" : Boolean   len 1 required 1 = install_css,
        static_method "svb" : Any len 1 required 1 = install_css,
        static_method "svh" : Any len 1 required 1 = install_css,
        static_method "svi" : Any len 1 required 1 = install_css,
        static_method "svmax" : Any len 1 required 1 = install_css,
        static_method "svmin" : Any len 1 required 1 = install_css,
        static_method "svw" : Any len 1 required 1 = install_css,
        static_method "turn" : Any len 1 required 1 = install_css,
        static_method "vb" : Any len 1 required 1 = install_css,
        static_method "vh" : Any len 1 required 1 = install_css,
        static_method "vi" : Any len 1 required 1 = install_css,
        static_method "vmax" : Any len 1 required 1 = install_css,
        static_method "vmin" : Any len 1 required 1 = install_css,
        static_method "vw" : Any len 1 required 1 = install_css,
        static_method "x" : Any len 1 required 1 = install_css
    }
}
