//! `core/css/css_style_declaration.rs` — extracted verbatim from bindings.rs (file-move STEP 1). Pure code move, no logic
//! change. Add `CppgcInherits`/`JsErrorBox`/... to the
//! imports below if the gate reports them missing.

use deno_core::v8::MapFnTo;
use deno_core::v8;
use korvus_dom::Dom;
use crate::binding::HostDom;

use crate::binding::protos::str_val;

pub(crate) fn priv_get<'s>(scope: &mut v8::PinScope<'s, '_>, o: v8::Local<v8::Object>, name: &str) -> Option<v8::Local<'s, v8::Value>> {
    let key = crate::binding::protos::private_key(scope, name);
    o.get_private(scope, key).filter(|v| !v.is_undefined())
}

/// Which of the three kinds of declaration `this` is: an element's inline block (`el.style`, its element), a computed
/// one (the element it resolves against, `None` for the always-empty kind), or neither (a rule's block).
#[derive(Clone, Copy)]
enum Kind {
    Element(u32),
    Computed(Option<u32>),
    Other,
}

/// [`Kind`] of `this`, from its ONE slot ([`CSD_KIND`]): an element's id as itself, a computed declaration's element
/// as `-(id + 1)`, the always-empty kind as `null`, nothing on a rule's block.
///
/// The kinds were two slots, and since every read asked the element's slot first (`csd_eid`, then `csd_computed`), a
/// read on a computed declaration or a rule's block looked the keys up twice and read two slots -- 450-700
/// instructions more per read than before that order, which fingerprinting scripts pay on every
/// `getComputedStyle(x)[i]` (followup-issues.json[13]: `cs[5]` 9,349 -> 10,072, `cs.cssText` 3,966 -> 4,415). One
/// slot is one key and one read for every kind.
fn csd_kind(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Kind {
    let key = cache(scope).kind;
    let Some(v) = this.get_private(scope, key) else { return Kind::Other };
    if v.is_null() {
        return Kind::Computed(None);
    }
    match v8::Local::<v8::Number>::try_from(v).map(|n| n.value()) {
        Ok(n) if n >= 0.0 => Kind::Element(n as u32),
        Ok(n) => Kind::Computed(Some((-n - 1.0) as u32)),
        Err(_) => Kind::Other,
    }
}

fn csd_eid(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Option<u32> {
    match csd_kind(scope, this) {
        Kind::Element(id) => Some(id),
        _ => None,
    }
}

/// What every declaration is keyed by, resolved ONCE per runtime; what it is minted from is
/// `CSSStyleDeclaration`'s template, whose instance template the builder gives the interceptors
/// ([`configure`]).
///
/// # Why this exists
///
/// `new_decl` used to build a fresh `FunctionTemplate` and interceptor configuration for every
/// declaration it minted, and `csd_computed` / `csd_eid` / `priv_set` resolved their `v8::Private`
/// by NAME on every call (`v8::String::new` + `Private::for_api`, a registry lookup). Measured on
/// the P9c fixture -- 12,004 elements, `getComputedStyle(e)` reading nothing -- in ns per call:
///
/// ```text
///   instanceof Element check      ~240
///   is_connected                  ~130
///   animation tick               ~1300-1900   (of which performance.now via JS ~500)
///   CSSStyleDeclaration lookup    ~240
///   new_decl (template per call) ~1300
///   priv_set (key by name)        ~320
///   total                        ~4000        Chrome 150: ~250
/// ```
///
/// A template per object is also a MAP per object: no two declarations shared a hidden class, so
/// every property read off one was megamorphic. An `ObjectTemplate` is isolate state, not object
/// state -- the builder makes each interface's once per isolate -- and a `v8::Private` for a given
/// name is the same symbol however many times it is asked for, so caching either changes no answer.
///
/// # What it does NOT cache
///
/// The object. `getComputedStyle(e) === getComputedStyle(e)` is `false` in Chrome 150 (measured
/// over CDP), and the declaration is LIVE: it carries the element and resolves on
/// access, so there is no value here to go stale.
struct CsdKeys {
    kind: v8::Global<v8::Private>,
}

/// The cached handles as locals in `scope`.
struct CsdLocal<'s> {
    kind: v8::Local<'s, v8::Private>,
}

thread_local! {
    static TEMPLATE_BUILDS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// How many times THIS THREAD has built the declaration template -- `CSSStyleDeclaration`'s, once per
/// isolate when the builder makes it ([`configure`]), however many declarations it mints. Read by
/// `korvus-runtime`'s `computed_style` test, which asserts it as a count; thread-local so
/// a test running beside it on another thread cannot move it.
pub fn declaration_template_builds() -> u64 {
    TEMPLATE_BUILDS.with(|c| c.get())
}

fn cache<'s>(scope: &mut v8::PinScope<'s, '_>) -> CsdLocal<'s> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    {
        let st = op_state.borrow();
        if let Some(t) = st.try_borrow::<CsdKeys>() {
            return CsdLocal { kind: v8::Local::new(scope, &t.kind) };
        }
    }
    let name = v8::String::new(scope, CSD_KIND).unwrap();
    let kind = v8::Private::for_api(scope, Some(name));
    op_state.borrow_mut().put(CsdKeys { kind: v8::Global::new(scope, kind) });
    CsdLocal { kind }
}

/// Whether `this` is a COMPUTED declaration, and which element it resolves against.
///
/// `Some(Some(id))` is `getComputedStyle(el)` for a connected element; `Some(None)` is the
/// always-empty kind (a pseudo-element this engine generates no box for); `None` is not a computed
/// declaration at all -- `el.style`, or a rule's block.
///
/// `getComputedStyle()` used to return a JS object literal wrapped in a `Proxy` -- so its five members
/// were plain JS closures (`String(getComputedStyle(el).getPropertyValue)` printed the source) and
/// `getComputedStyle(el) instanceof CSSStyleDeclaration` was FALSE, which it is true for in every
/// browser. It is now the same platform object `el.style` is, distinguished only by carrying an
/// element to RESOLVE against rather than one to read a `style` attribute off.
///
/// IT CARRIES AN ELEMENT AND NOT A SNAPSHOT, and that is a fidelity fix as well as the
/// reason the call got cheap. Chrome's result is LIVE, measured on Chrome 150 over CDP:
///
/// ```text
///   const s = getComputedStyle(el);  el.style.color = 'red';  s.color
///   chrome  rgb(0, 0, 255)  ->  rgb(255, 0, 0)      korvus (before)  stayed blue
///   s.length after el.style.setProperty('--x','1px')
///   chrome  474 -> 475                              korvus (before)  frozen
///   s.length after el.remove() / after appending a detached el
///   chrome  474 -> 0     and   0 -> 474             korvus (before)  frozen at its first answer
/// ```
///
/// The old eager pack froze the answer at the moment of the call, so a page that read a style,
/// changed it and read the same object back saw the OLD value. Resolving on access is what a live
/// object is.
pub(crate) fn csd_computed(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Option<Option<u32>> {
    match csd_kind(scope, this) {
        Kind::Computed(node) => Some(node),
        _ => None,
    }
}

/// The resolved value of `prop` (already dashed) on a computed declaration, or "" -- the answer for a
/// name this engine does not know, which is also what a browser gives for a name that is not a CSS
/// property at all (Chrome's `getPropertyValue("colr")` is `""`, measured).
///
/// A read is a style flush (Blink's `GetPropertyCSSValue` -> `UpdateStyleAndLayoutTreeForElement`): the animation
/// tick runs here, not at the `getComputedStyle` call (`animation::css::flush_style_for`). It comes before the value
/// is read, because an animated property's value is what the style is supposed to carry -- an element appended with a
/// paused `animation` reads its first keyframe's value at once (Chrome 150).
fn computed_value(scope: &mut v8::PinScope, node: Option<u32>, prop: &str) -> String {
    let Some(id) = node else { return String::new() };
    crate::core::animation::css::flush_style_for(scope, id);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    crate::core::css::cssom::css_computed_one(&mut st, id, prop)
}

/// A computed declaration is read-only: CSSOM makes every mutating member raise `NoModificationAllowedError` --
/// AFTER its arguments are converted, with Blink's message (`CSSComputedStyleDeclaration`). Chrome 152 (`c3b.html`,
/// 2026-10-23): `cs.setProperty('WIDTH', '1px')` throws "Failed to execute 'setProperty' on 'CSSStyleDeclaration':
/// These styles are computed, and therefore the 'WIDTH' property is read-only.", `cs.cssFloat = 'left'` "Failed to
/// set the 'cssFloat' property on ...: ... the 'float' property is read-only.", `cs.cssText = ''` "Failed to set the
/// 'cssText' property on ...: These styles are computed, and therefore read-only."; korvus named the member ("the
/// 'setProperty' method is readonly") and threw before converting anything. `what`: the operation or attribute,
/// `property`: the property it was asked about. Returns true when the caller must stop.
fn computed_refuses(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, what: Refused, property: Option<&str>) -> bool {
    if csd_computed(scope, this).is_none() {
        return false;
    }
    let op = match what {
        Refused::Execute(op) => format!("execute '{op}'"),
        Refused::Set(attribute) => format!("set the '{attribute}' property"),
    };
    let tail = match property {
        Some(p) => format!("the '{p}' property is read-only."),
        None => "read-only.".to_string(),
    };
    crate::binding::protos::throw_dom(
        scope,
        &format!("Failed to {op} on 'CSSStyleDeclaration': These styles are computed, and therefore {tail}"),
        "NoModificationAllowedError",
    );
    true
}

/// What a computed declaration refused ([`computed_refuses`]): an operation, or an attribute's setter.
#[derive(Clone, Copy)]
enum Refused {
    Execute(&'static str),
    Set(&'static str),
}

/// A `CSSOMString` argument converted as WebIDL converts it (ToString), `None` when the conversion threw -- the
/// exception is left pending and the caller returns with it, before it converts anything else or changes anything.
/// Chrome 152 (`c3.html`, 2026-10-23): `style.setProperty(n, v)` whose `n` throws from its `Symbol.toPrimitive`
/// throws that error with `v` never converted and nothing written; korvus converted the value too, wrote it, and
/// threw nothing -- and when `v` threw, removed the declaration. `legacy_null`: `[LegacyNullToEmptyString]`, `null`
/// is the empty string.
fn css_string<'b>(
    scope: &mut v8::PinScope,
    v: v8::Local<v8::Value>,
    legacy_null: bool,
    buf: &'b mut [std::mem::MaybeUninit<u8>; 64],
) -> Option<std::borrow::Cow<'b, str>> {
    if legacy_null && v.is_null() {
        return Some(std::borrow::Cow::Borrowed(""));
    }
    if let Ok(s) = v8::Local::<v8::String>::try_from(v) {
        return Some(s.to_rust_cow_lossy(scope, buf));
    }
    let s = v.to_string(scope)?;
    Some(s.to_rust_string_lossy(scope).into())
}
/// An element's inline `color` declaration as written, `None` when it has no `style` attribute -- what a
/// `<canvas>`'s `currentColor` reads (Blink: `InlineStyle()->GetPropertyValue(CSSPropertyID::kColor)`, then
/// `CSSParser::ParseColor`). An `!important` flag is not part of the value.
pub(crate) fn inline_color(dom: &Dom, id: u32) -> Option<String> {
    dom.get_attribute(id, "style")?;
    let v = style_decls(dom, id).into_iter().rev().find(|(k, _)| k.eq_ignore_ascii_case("color")).map(|(_, v)| v).unwrap_or_default();
    let v = v.trim();
    let lower = v.to_ascii_lowercase();
    Some(match lower.rfind('!') {
        Some(i) if lower[i + 1..].trim() == "important" => v[..i].trim().to_string(),
        _ => v.to_string(),
    })
}

fn style_decls(dom: &Dom, id: u32) -> Vec<(String, String)> {
    split_decls(dom.get_attribute(id, "style"))
}

/// The declarations the cascade reads ([`Dom::style_for_cascade`]): the attribute's, each value the CSSOM set at
/// the precision the page wrote it.
fn precise_decls(dom: &Dom, id: u32) -> Vec<(String, String)> {
    split_decls(dom.style_for_cascade(id))
}

fn split_decls(text: Option<&str>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(s) = text {
        each_declaration(s, |d| {
            let Some(i) = d.find(':') else { return };
            let k = d[..i].trim();
            if !k.is_empty() {
                out.push((k.to_string(), d[i + 1..].trim().to_string()));
            }
        });
    }
    out
}

/// `f` on each declaration of `text`, cut where the tokenizer ends it: at each `;` [`scan`] finds at the top level.
///
/// Cut at EVERY `;`, `background-image: url(data:image/png;base64,...)` -- common in `style` attributes -- was two
/// declarations: the next CSSOM write to any property left the cascade `url(data:image/png)`, its computed value
/// and its fetch, and `cssText = ...` declared it so, where Chrome 152 keeps the whole URL (2026-10-23).
fn each_declaration<'t>(text: &'t str, mut f: impl FnMut(&'t str)) {
    let mut start = 0;
    scan(text, |i| {
        f(&text[start..i]);
        start = i + 1;
    });
    f(&text[start..]);
}

/// [`each_declaration`]'s pieces.
fn declarations(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    each_declaration(text, |d| out.push(d));
    out
}

/// What the tokenizer leaves open at the end of a text ([`scan`]).
#[derive(Default)]
struct Open {
    /// the closers of the blocks still open, innermost last
    blocks: Vec<u8>,
    quote: Option<u8>,
    /// an unquoted `url(`, which only its `)` ends
    url: bool,
    comment: bool,
    /// a `\` with nothing after it
    escape: bool,
}

/// `text` read as the CSS tokenizer reads it, as far as where a declaration ends: `at` is called with each `;` at
/// the top level -- outside strings, comments, unquoted `url(...)`s and blocks, and not escaped -- and what is left
/// open at the end is answered. [`each_declaration`] cuts at those, and [`closed`] closes what a value leaves open
/// by the same reading, so the list [`decls_text`] writes splits back into its declarations. As the tokenizer: a
/// newline ends a string (a bad string), an unquoted url runs to its first unescaped `)` whatever it holds (a bad
/// url too), and a block ends only at its own closer.
fn scan(text: &str, mut at: impl FnMut(usize)) -> Open {
    // every byte but a STOP is passed over in one test, and a string, a comment and a url are each read to their end
    // in a loop of their own: [`decls_text`] reads the whole list on every write, and one `match` per byte over every
    // state cost a four-declaration write 0.6k instructions more than the cut at every `;` (2026-10-23)
    let b = text.as_bytes();
    let mut o = Open::default();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        i += 1;
        if !STOP[c as usize] {
            continue;
        }
        match c {
            // the next character is escaped, whatever it is
            b'\\' => {
                o.escape = i == b.len();
                i += 1;
            }
            // to the closing quote, or a newline (a bad string); a backslash before a newline continues the string,
            // and CRLF is one newline
            b'"' | b'\'' => loop {
                let Some(&d) = b.get(i) else {
                    o.quote = Some(c);
                    break;
                };
                i += 1;
                if d == c || matches!(d, b'\n' | b'\r' | b'\x0c') {
                    break;
                }
                if d == b'\\' {
                    match b.get(i) {
                        None => {
                            (o.quote, o.escape) = (Some(c), true);
                            break;
                        }
                        Some(b'\r') if b.get(i + 1) == Some(&b'\n') => i += 2,
                        Some(_) => i += 1,
                    }
                }
            },
            b'/' if b.get(i) == Some(&b'*') => match text[i + 1..].find("*/") {
                Some(end) => i += 1 + end + 2,
                None => {
                    o.comment = true;
                    i = b.len();
                }
            },
            // an unquoted url: to its first unescaped `)`, whatever it holds
            b'(' if unquoted_url(b, i - 1) => loop {
                let Some(&d) = b.get(i) else {
                    o.url = true;
                    break;
                };
                i += 1;
                if d == b')' {
                    break;
                }
                if d == b'\\' {
                    if i == b.len() {
                        (o.url, o.escape) = (true, true);
                        break;
                    }
                    i += 1;
                }
            },
            b'(' => o.blocks.push(b')'),
            b'[' => o.blocks.push(b']'),
            b'{' => o.blocks.push(b'}'),
            b')' | b']' | b'}' if o.blocks.last() == Some(&c) => {
                o.blocks.pop();
            }
            b';' if o.blocks.is_empty() => at(i - 1),
            _ => {}
        }
    }
    o
}

/// The bytes that start or end something at the top level of a declaration ([`scan`]).
const STOP: [bool; 256] = {
    let mut t = [false; 256];
    let mut k = 0;
    let stops = b"\\\"'/()[]{};";
    while k < stops.len() {
        t[stops[k] as usize] = true;
        k += 1;
    }
    t
};

/// Is the `(` at `b[i]` an unquoted url's: the identifier before it is `url` -- not the end of a longer name, a hash
/// or an at-keyword -- and no quote follows it (`url("...")` is a function holding a string).
fn unquoted_url(b: &[u8], i: usize) -> bool {
    let name = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'#' | b'@') || c >= 0x80;
    i >= 3
        && b[i - 3..i].eq_ignore_ascii_case(b"url")
        && (i == 3 || !name(b[i - 4]))
        && !matches!(b[i + 1..].iter().find(|c| !matches!(c, b' ' | b'\t' | b'\n' | b'\r' | b'\x0c')), Some(b'"' | b'\''))
}

/// A value as an inline declaration holds it. The declarations here are the `style` attribute's own
/// text, split -- not the page parser's -- so a property whose value Chrome keeps in a CANONICAL form
/// is put in it on the way in and out: `font-family` (`page::style::font_family`, the
/// same function the cascade stores it through). `None` is a value that does not parse, which makes
/// the declaration absent.
fn inline_value(prop: &str, raw: &str) -> Option<String> {
    if prop == "font-family" {
        return page::style::font_family::canonical(raw);
    }
    // A custom property keeps its tokens as written (only trimmed), as Chrome does.
    if prop.starts_with("--") {
        return Some(raw.trim().to_string());
    }
    // Every math function first, from the page's own text: Blink parses and simplifies it at full
    // double precision and writes the result (`page::style::calc`) -- `calc(1px * sin(0.5))` reads back
    // `calc(0.479426px)` -- where the token pass below cuts every number to six digits, which is not the value
    // Fingerprint's `sin(66911823500 * ...)` is a function of.
    let raw = page::style::calc::serialize_math_in(raw);
    // Then the value's own canonical form: colours as `rgb()`, lengths with units, `calc()`
    // simplified, a shadow's colour first -- `page::style::declared` has Chrome's table.
    Some(page::style::declared::canonical(prop, &canonical_tokens(&raw)))
}

/// A declared value's TOKEN-LEVEL canonical serialization (Chrome 150): whitespace runs collapse,
/// none follows `(` or precedes `)` or `,`, a comma is followed by exactly one space, and a number is
/// re-serialized (`.5` -> `0.5`, `1e1px` -> `10px`, at most 6 significant digits). Strings and `url(...)` keep
/// their text. anime.js splits `style.transform` on ") translate(": korvus handed back `translate(12px,34px)`.
fn canonical_tokens(raw: &str) -> String {
    let t = raw.trim();
    let b = t.as_bytes();
    let mut out = String::with_capacity(b.len());
    let mut pending_space = false;
    let mut i = 0;
    let push = |out: &mut String, pending: &mut bool, tok: &str| {
        let opens_or_starts = out.is_empty() || out.ends_with('(') || out.ends_with(' ');
        // Component values are space-separated: `scale(1)translateZ(2px)` reads back `scale(1) translateZ(2px)`
        // (Chrome 150; animejs.com builds its transforms without spaces).
        let after_close = out.ends_with(')');
        if (*pending || after_close) && !opens_or_starts && !tok.starts_with(')') && !tok.starts_with(',') {
            out.push(' ');
        }
        *pending = false;
        out.push_str(tok);
    };
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            pending_space = true;
            i += 1;
            continue;
        }
        if c == b'"' || c == b'\'' {
            let start = i;
            i += 1;
            while i < b.len() && b[i] != c {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i = (i + 1).min(b.len());
            push(&mut out, &mut pending_space, &t[start..i]);
            continue;
        }
        if c == b',' {
            push(&mut out, &mut pending_space, ",");
            pending_space = true;
            i += 1;
            continue;
        }
        if c == b'(' || c == b')' {
            push(&mut out, &mut pending_space, if c == b'(' { "(" } else { ")" });
            i += 1;
            continue;
        }
        // a number: [+-]? digits [. digits] | . digits, [e[+-]digits], then its unit or %
        let num_start = c.is_ascii_digit()
            || (c == b'.' && b.get(i + 1).is_some_and(|d| d.is_ascii_digit()))
            || ((c == b'+' || c == b'-')
                && (b.get(i + 1).is_some_and(|d| d.is_ascii_digit())
                    || (b.get(i + 1) == Some(&b'.') && b.get(i + 2).is_some_and(|d| d.is_ascii_digit()))));
        if num_start {
            let start = i;
            if c == b'+' || c == b'-' {
                i += 1;
            }
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            if i < b.len() && b[i] == b'.' && b.get(i + 1).is_some_and(|d| d.is_ascii_digit()) {
                i += 1;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
            }
            if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
                let mut j = i + 1;
                if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
                    j += 1;
                }
                if j < b.len() && b[j].is_ascii_digit() {
                    i = j;
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                }
            }
            let num = &t[start..i];
            let unit_start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'%' || b[i] == b'-' || b[i] == b'_') {
                i += 1;
            }
            let unit = &t[unit_start..i];
            let tok = match num.parse::<f64>() {
                Ok(n) => format!("{}{}", css_number(n), unit),
                Err(_) => format!("{num}{unit}"),
            };
            push(&mut out, &mut pending_space, &tok);
            continue;
        }
        // an identifier, function name, hash, delimiter: up to the next structural character
        let start = i;
        let is_url = b.len() - i >= 4 && b[i..i + 4].eq_ignore_ascii_case(b"url(");
        if is_url {
            // `url(...)` verbatim through its closing parenthesis
            while i < b.len() && b[i] != b')' {
                i += 1;
            }
            i = (i + 1).min(b.len());
            push(&mut out, &mut pending_space, &t[start..i]);
            continue;
        }
        while i < b.len() && !b[i].is_ascii_whitespace() && !matches!(b[i], b',' | b'(' | b')' | b'"' | b'\'') {
            i += 1;
        }
        if i == start {
            i += 1;
        }
        push(&mut out, &mut pending_space, &t[start..i]);
    }
    out
}

/// A CSS number as Chrome serializes a declared one (`CSSNumericLiteralValue::CustomCSSText`): an integer within
/// +-999999 as itself, anything else as C's `%.6g`. Chrome 150 reads `style.opacity = "1234567.5"` back as
/// `1.23457e+06` and `scale(0.000012345)` as `scale(1.2345e-05)`; this wrote `1234570` and `0.000012345`.
fn css_number(n: f64) -> String {
    page::style::serialize::format_number(n)
}

/// One declaration of a `cssText` write, `prop: value` (`value` may carry its `!important`): appended to `decls` in
/// its canonical form -- so the attribute Chrome writes back reads the same -- and to `precise` as the page wrote it,
/// when the property accepts it (`CSS.supports`'s own test; an empty value is no declaration).
///
/// Every accepted declaration, in order, and nothing replaced or removed: `ParseDeclarationList` keeps them all and
/// filters them once at the end -- the last declaration of each longhand winning, an `!important` one over the
/// rest. A later declaration of a property replaced the earlier one where it stood, so a shorthand written between
/// the two won: Chrome 152 reads `cssText = 'margin-left: 7px; margin: 3px; margin-left: 9px'` as `margin: 3px 3px 3px
/// 9px;`, this `margin: 3px;`; `width: 1px !important; width: 2px` as `width: 1px !important;`, this `width: 2px;`;
/// and `width: 1px; width: ` as `width: 1px;`, where the empty one removed it (`f4css.html`, 2026-10-23).
fn css_text_declaration(decls: &mut Vec<(String, String)>, precise: &mut Vec<(String, String)>, prop: String, value: &str) {
    let value = value.trim();
    if value.is_empty() || !page::style::supports::supports_declaration(&prop, value) {
        return;
    }
    let Some(canonical) = inline_value(&prop, value) else { return };
    precise.push((prop.clone(), value.to_string()));
    decls.push((prop, canonical));
}

/// Whether `value` holds a `!` outside its strings and `url(...)`s: a `!important` the property's grammar never
/// takes.
fn bang_outside_strings(value: &str) -> bool {
    let b = value.as_bytes();
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < b.len() {
        let c = b[i];
        match quote {
            Some(q) => {
                if c == b'\\' {
                    i += 1;
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                b'"' | b'\'' => quote = Some(c),
                b'!' => return true,
                b'(' if i >= 3 && b[i - 3..i].eq_ignore_ascii_case(b"url") => {
                    // an unquoted url's text, up to its parenthesis
                    while i < b.len() && b[i] != b')' {
                        i += 1;
                    }
                }
                _ => {}
            },
        }
        i += 1;
    }
    false
}


/// `prop: value;` per declaration, space-separated -- the text a declaration list is kept as, what each declaration
/// leaves open closed so it ends where its `;` is ([`closed`]) -- and whether [`split_decls`] reads exactly `d` back
/// from it: every name non-empty, trimmed and free of `:` and of what [`scan`] stops at, every value trimmed, every
/// declaration closed already and without a `;` of its own at the top level (one inside a string or a `url()` stays
/// where it is).
fn decls_text(d: &[(String, String)], extra: usize) -> (String, bool) {
    // one buffer: a `format!` and a `closed` copy per declaration, a Vec and a join were 1.3k of a 34k-instruction
    // `el.style.width = v`, 17k of four longhand writes (2026-10-23); `extra` more for what follows it
    let mut out = String::with_capacity(d.iter().map(|(k, v)| k.len() + v.len() + 4).sum::<usize>() + extra);
    let mut exact = true;
    for (i, (k, v)) in d.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(k);
        out.push_str(": ");
        out.push_str(v);
        // the declaration as the list's tokenizer reads it: from its value when its name starts nothing (a property's
        // never does), else whole
        let plain = !k.bytes().any(|c| c == b':' || STOP[c as usize]);
        let at = out.len() - v.len() - if plain { 0 } else { k.len() + 2 };
        let mut cut = false;
        let open = scan(&out[at..], |_| cut = true);
        let end = out.len();
        close(&mut out, &open);
        exact &= plain && !k.is_empty() && k.trim() == k && v.trim() == v.as_str() && !cut && out.len() == end;
        out.push(';');
    }
    (out, exact)
}

/// The element's inline declaration block as Blink holds it (a bot-detection script D6): the block the CSSOM write that left its
/// `style` attribute made, else that attribute parsed into longhands (`page::style::inline`). Every inline read answers from it -- `cssText`, `length`,
/// `item()`, `getPropertyValue`, `getPropertyPriority`, the named properties -- where they answered the attribute's
/// own text: Chrome 150 reads `setAttribute('style', 'border: 0')` back as `border: 0px;`, seventeen longhands, and
/// `borderTopWidth` `0px`; this read `border: 0`, one name, and ``.
fn inline_block(dom: &Dom, id: u32) -> page::style::inline::InlineStyle {
    let attribute = dom.get_attribute(id, "style").unwrap_or("");
    page::style::inline::InlineStyle::written_block(dom.style_for_cascade(id).unwrap_or(""), attribute)
}

/// `f` on [`inline_block`]'s block, read in place: a read never needs its own copy. The element's own block
/// ([`Slot`]) while it holds, else the block its attribute's text stands for.
fn with_inline<R>(dom: &Dom, id: u32, f: impl FnOnce(&page::style::inline::InlineStyle) -> R) -> R {
    let key = (dom.instance(), id);
    // `f` reads the block and never this map
    let f = match BLOCKS.with(|b| match b.borrow().get(&key) {
        Some(slot) if slot.holds(dom, id) => Ok(f(&slot.block)),
        _ => Err(f),
    }) {
        Ok(r) => return r,
        Err(f) => f,
    };
    let attribute = dom.get_attribute(id, "style").unwrap_or("");
    let (r, own) = page::style::inline::InlineStyle::with_written(dom.style_for_cascade(id).unwrap_or(""), attribute, f);
    // a block too large to share by its text (`inline::SHARED_BYTES`) is the element's own to keep, as Blink keeps
    // each element's: elements read in turn each read their own, where a map of the texts held a few of them
    if let Some(block) = own {
        keep_slot(dom, id, block, None, false);
    }
    r
}

/// An element's inline block as its last CSSOM write left it, beside the attribute that write serialized it to --
/// Blink's `inline_style_`, which the element keeps and only serializes into the attribute.
///
/// Valid while `stamp` is the element's [`Dom::style_writes_of`]: every later write of another text to the attribute,
/// or its removal, moves that count (`set_attribute` and `remove_attribute` are the only writers of a made element's
/// attributes), ids are never reused and `Dom::instance` is process-unique, so a matching `(instance, id, stamp)` says
/// the attribute is exactly the text the write left. (The text it holds written again moves nothing: Blink keeps the
/// block then, as `Dom::set_attribute` keeps the cascade's text.) The block was re-derived from that text through
/// `InlineStyle::parse_attribute`, which hashed the whole text and cloned the block on every read and write; the
/// two differ only where the text-keyed copy was another element's (one whose write serialized to the same text
/// later: Chrome 152 reads each element's own) or was dropped at its 4096-entry clear and re-parsed.
///
/// `precise`: the cascade's declarations ([`precise_decls`]) when [`decls_text`] wrote them exactly, so the next
/// write need not split [`Dom::style_for_cascade`] back into them; `disjoint`: no two of them share a longhand
/// ([`shares_none`]), so a write of one of them replaces it where it stands without expanding a name ([`respell`]).
///
/// Also the block of an element no CSSOM write made, read off an attribute too large to share by its text
/// ([`with_inline`]). `cost`: the bytes of its texts, counted against `page::style::memo::ELEMENT_BYTES`.
struct Slot {
    stamp: u32,
    block: page::style::inline::InlineStyle,
    precise: Option<Vec<(String, String)>>,
    disjoint: bool,
    cost: usize,
}

impl Slot {
    /// Is this still `id`'s block? While its stamp is the element's style-write count -- through any number of writes
    /// of the text the attribute holds, which `Dom` does not count. (It held through ONE such write, by its text: two
    /// in a row, or a clone made after one, read the parse's order where Chrome 152 reads the setters', `rv3a.html`.)
    fn holds(&self, dom: &Dom, id: u32) -> bool {
        self.stamp == dom.style_writes_of(id)
    }
}

thread_local! {
    /// [`Slot`]s by `(Dom::instance, element)`, bounded by clearing (an evicted element reads as before): at 8192 of
    /// them, or past `ELEMENT_BYTES` of their texts ([`BLOCKS_BYTES`]).
    static BLOCKS: std::cell::RefCell<korvus_dom::FxHashMap<(u64, u32), Slot>> = Default::default();
    /// The bytes [`BLOCKS`]' slots hold ([`Slot::cost`]).
    static BLOCKS_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The block and the cascade's declarations a write to `id` starts from: its [`Slot`] taken out while it holds,
/// else read as before. `Some(listed)` when they came from the slot, `listed` whether it kept the list; and whether
/// no two of the declarations share a longhand ([`Slot::disjoint`]).
fn take_slot(dom: &Dom, id: u32) -> (page::style::inline::InlineStyle, Vec<(String, String)>, Option<bool>, bool) {
    let key = (dom.instance(), id);
    let slot = BLOCKS.with(|b| {
        let mut b = b.borrow_mut();
        match b.get(&key).map(|s| s.holds(dom, id)) {
            Some(true) => b.remove(&key).inspect(|s| BLOCKS_BYTES.with(|n| n.set(n.get().saturating_sub(s.cost)))),
            _ => None,
        }
    });
    match slot {
        Some(Slot { block, precise: Some(list), disjoint, .. }) => (block, list, Some(true), disjoint),
        Some(Slot { block, precise: None, .. }) => {
            let list = precise_decls(dom, id);
            let d = shares_none(&list);
            (block, list, Some(false), d)
        }
        None => {
            let list = precise_decls(dom, id);
            let d = shares_none(&list);
            (inline_block(dom, id), list, None, d)
        }
    }
}

/// Keep `block` as `id`'s [`Slot`], stamped with its style-write count as it is now, with the cascade's declarations
/// `precise` when they are kept, `disjoint` as [`Slot::disjoint`] says.
fn keep_slot(dom: &Dom, id: u32, block: page::style::inline::InlineStyle, precise: Option<Vec<(String, String)>>, disjoint: bool) {
    if dom.get_attribute(id, "style").is_none() {
        return;
    }
    let cost = block.bytes() + precise.as_ref().map_or(0, |p| p.iter().map(|(k, v)| k.len() + v.len()).sum());
    let slot = Slot { stamp: dom.style_writes_of(id), block, precise, disjoint, cost };
    BLOCKS.with(|b| {
        let mut b = b.borrow_mut();
        BLOCKS_BYTES.with(|n| {
            if b.len() >= 8192 || n.get() + cost > page::style::memo::ELEMENT_BYTES {
                b.clear();
                n.set(0);
            }
            let replaced = b.insert((dom.instance(), id), slot).map_or(0, |s| s.cost);
            n.set(n.get().saturating_sub(replaced) + cost);
        });
    });
}

/// Write `block` back: the attribute is its serialization (Blink writes the attribute from the parsed block after
/// every CSSOM change), the cascade keeps `precise`, the declarations as the page wrote
/// them. Whether the cascade's text reads back as `precise` exactly ([`decls_text`]).
fn style_write(dom: &mut Dom, id: u32, block: &page::style::inline::InlineStyle, precise: &[(String, String)]) -> bool {
    let (attribute, declarations) = block.css_text_counted();
    let (mut text, exact) = decls_text(precise, block.order_note_bytes(declarations));
    // the block's order goes with the cascade's text, which the element and its clones carry
    // (`InlineStyle::note_order`): what a read finds once the element's slot is gone, or on a clone
    block.note_order(&mut text, declarations);
    dom.set_inline_style(id, &attribute, text);
    exact
}

/// [`style_write`] for a write that changed neither the element's block nor the cascade's declarations ([`Slot`]): the
/// attribute is that block's serialization and the cascade's text is that list's, so the same two texts are written
/// back -- the attribute write's bookkeeping exactly as before (a revision: caches keyed on it, and an XPath iterator
/// over the document, see a write) -- without serializing the block or the list again. A page re-applying the same
/// styles every frame paid both on every write (2026-10-23).
fn rewrite_unchanged(dom: &mut Dom, id: u32) {
    let attribute = dom.get_attribute(id, "style").unwrap_or("").to_string();
    let cascade = dom.style_for_cascade(id).unwrap_or("").to_string();
    dom.set_inline_style(id, &attribute, cascade);
}

/// The setter's checks on a non-empty, trimmed `raw` -- no `!` in the value, a value the property accepts, one
/// [`inline_value`] makes -- and the canonical value the block stores; `None` when the write changes nothing. Chrome
/// 150, measured: a value the property does not accept is ignored -- the declaration keeps what it had. korvus stored
/// any string, so worldbank.org's `style.display = null` wrote `display: null`, the computed display read "null", and
/// 80 % of the page's text left innerText. Validity is `CSS.supports`'s own test.
///
/// A function of its three inputs alone (every callee is a function of its strings), so the recent answers are kept:
/// a page writes the same few values over and over (toggles, cycles, frameworks re-applying a style every frame), and
/// each answer is a full parse of the declaration plus its canonicalisation -- ~0.4 us of a 2.8 us `el.style.x = v`
/// against Chrome 152's 0.2 (2026-10-23). Keyed by the inputs' hash and verified against the inputs themselves, so a
/// collision only recomputes; bounded by clearing.
fn accept(prop: &str, raw: &str, important: bool) -> Option<String> {
    use std::hash::BuildHasher;
    type Seen = korvus_dom::FxHashMap<u64, (Box<str>, Box<str>, bool, Option<Box<str>>)>;
    thread_local! {
        static ACCEPTED: std::cell::RefCell<Seen> = Default::default();
        static ACCEPTED_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    let key = ACCEPTED.with(|m| m.borrow().hasher().hash_one((prop, raw, important)));
    let hit = ACCEPTED.with(|m| {
        m.borrow().get(&key).filter(|e| &*e.0 == prop && &*e.1 == raw && e.2 == important).map(|e| e.3.as_deref().map(str::to_string))
    });
    if let Some(answer) = hit {
        return answer;
    }
    let answer = accept_uncached(prop, raw, important);
    ACCEPTED.with(|m| {
        let mut m = m.borrow_mut();
        // bounded by its bytes too (`page::style::memo`): a large value is answered, not kept
        let cost = prop.len() + raw.len() + answer.as_ref().map_or(0, String::len);
        if ACCEPTED_BYTES.with(|b| page::style::memo::admit(m.len(), 1024, b, cost, || m.clear())) {
            m.insert(key, (prop.into(), raw.into(), important, answer.as_deref().map(Box::from)));
        }
    });
    answer
}

fn accept_uncached(prop: &str, raw: &str, important: bool) -> Option<String> {
    let written = if important { std::borrow::Cow::Owned(format!("{raw} !important")) } else { std::borrow::Cow::Borrowed(raw) };
    // A priority is the setter's own argument, never part of the value: Chrome 150 ignores `style.fontSize = "150px
    // !important"` (a bot challenge's SVG texts stay 16px there) -- this set 150px, important.
    if !important && bang_outside_strings(&written) {
        return None;
    }
    if !page::style::supports::supports_declaration(prop, &written) {
        return None;
    }
    let v = inline_value(prop, &written)?;
    if important { inline_value(prop, raw) } else { Some(v) }
}

/// A CSSOM write to element `id`'s inline block, with the `style` attribute mutation it is -- its MutationObserver
/// record and custom-element reaction (`mutation_observer::style_attribute_mutation`): `always` for `cssText`,
/// `reacts` whether the write runs the reaction here.
///
/// Only the named setter and `cssFloat` run it. `setProperty`, `removeProperty` and the `cssText` setter are also how
/// a Typed OM write reaches the block (`style_property_map::cb_spm_write` calls them), and `StylePropertyMap` has no
/// [CEReactions]: Chrome 152 puts that reaction on the backup element queue, for the next microtask checkpoint
/// (2026-10-23, probe r2/l3/rr2/www/r1_a3c.html: `attributeStyleMap.set('color', 'red')` and `.delete('color')` on a
/// custom element observing `style` log nothing synchronously, `null>"color: red;"` and `"color: red;">""` in a
/// microtask). Run here, those fired synchronously, which korvus never did before; this file cannot tell the two
/// callers apart, so the three run none, as before the record existed, until a backup element queue takes the Typed
/// OM's (Chrome runs a page's own `setProperty` reaction synchronously).
fn mutate_inline(scope: &mut v8::PinScope, id: u32, always: bool, reacts: impl FnOnce(&mut v8::PinScope) -> bool, write: impl FnOnce(&mut Dom)) {
    crate::core::dom::mutation_observer::style_attribute_mutation(scope, id, always, reacts, |dom, id| with_inline(dom, id, |b| b.css_text()), write);
    // a style write can show a deferred `loading=lazy` element (`el.style.display = "block"` on a carousel slide)
    crate::core::html::lazy_load::invalidated(scope);
}

/// One CSSOM write of `prop` to an element's inline block -- `setProperty`, a named property, `cssFloat`; an empty
/// value removes it -- as `MutableCSSPropertyValueSet::SetProperty` makes it: the setter's checks first ([`accept`]:
/// no `!` in the value, a value the property accepts), then each longhand replaced where it stands or appended.
fn inline_write(dom: &mut Dom, id: u32, prop: &str, value: &str, important: bool) {
    let raw = value.trim();
    if raw.is_empty() {
        let (mut block, mut precise, _, mut disjoint) = take_slot(dom, id);
        block.remove(prop);
        // the cascade's text has to lose it too: the page's own declaration of it goes (see [`respell`])
        respell(&mut precise, &mut disjoint, prop, None, &block);
        let exact = style_write(dom, id, &block, &precise);
        keep_slot(dom, id, block, exact.then_some(precise), disjoint);
        return;
    }
    // the checks come before the block is read: a rejected value reads nothing (the reads only fill a cache of
    // the attribute's parse, a function of its text)
    let Some(canonical) = accept(prop, raw, important) else { return };
    let (mut block, mut precise, cached, mut disjoint) = take_slot(dom, id);
    let outcome = block.set_outcome(prop, &canonical, important);
    if outcome == page::style::inline::SetOutcome::Rejected {
        // nothing changed: a slot goes back as it was
        if let Some(listed) = cached {
            keep_slot(dom, id, block, listed.then_some(precise), disjoint);
        }
        return;
    }
    // the cascade's copy is one declaration of a list, so the value's blocks are closed first (see [`closed`])
    let kept = if important { format!("{} !important", closed(raw)) } else { closed(raw) };
    // Blink's kUnchanged: the block already held the value and the cascade's list holds `kept` where this write puts
    // it, so the attribute and the cascade's text stay what the slot says they are
    if outcome == page::style::inline::SetOutcome::Unchanged
        && cached == Some(true)
        && precise.iter().find(|(k, _)| k == prop).is_some_and(|(_, v)| *v == kept)
    {
        rewrite_unchanged(dom, id);
        keep_slot(dom, id, block, Some(precise), disjoint);
        return;
    }
    respell(&mut precise, &mut disjoint, prop, Some(kept), &block);
    let exact = style_write(dom, id, &block, &precise);
    keep_slot(dom, id, block, exact.then_some(precise), disjoint);
}

/// The cascade's declarations after a write of `prop` left `block` -- `kept`, the declaration as the page wrote it, or
/// `None` for a removal -- so that each longhand's last declaration in them (an `!important` one before the rest) is
/// the block's: where no other declaration shares a longhand with `prop`'s, its own is replaced where it stands,
/// appended, or removed, as Blink's block does; where one does, those declarations -- and any sharing a longhand
/// with them in turn -- are written again from the block, their longhands alone.
///
/// The setter path replaced `prop`'s declaration where it stood whatever stood after it, so a shorthand written
/// after a longhand of it won over the longhand written last: after `marginLeft = '7px'; margin = '3px'; marginLeft =
/// '9px'`, Chrome 152 computes `margin-left` 9px and lays it out at 9, this read 3px; `margin = '3px'; marginLeft =
/// '9px'; margin = '5px'` 5px (this 9px); `setProperty('margin-left', '9px', 'important')` then `margin = '3px'` 3px
/// (this 9px); the same for padding, border and inset, after a `margin` set `!important`, and after a `style`
/// attribute or `cssText` that declared both (`rv3c.html`, `f4css.html`, 2026-10-23). `cssText` read right throughout:
/// the block was, and only the cascade's text was not. The removal path wrote the whole list again from the block,
/// every other declaration's value at the attribute's precision with it: beside `transform: scale(1.000998)`,
/// `removeProperty('margin-left')` after `margin` laid the box out 1001px wide where Chrome keeps 1000.998
/// (`f4prec.html`).
///
/// `disjoint`: no two of `precise`'s declarations share a longhand -- then `prop`'s own declaration among them shares
/// none with the others, and is replaced or removed with no name expanded: the common write, `el.style.width = v`
/// beside a few others, which expanding every name made 15% slower (2026-10-23). It stays so through every change
/// made here, and is asked again of a list written again from the block.
fn respell(
    precise: &mut Vec<(String, String)>,
    disjoint: &mut bool,
    prop: &str,
    kept: Option<String>,
    block: &page::style::inline::InlineStyle,
) {
    let own = precise.iter().position(|(k, _)| k == prop);
    if (own.is_some() && *disjoint) || alone(precise, prop) {
        match (own, kept) {
            (Some(i), Some(kept)) => precise[i].1 = kept,
            (None, Some(kept)) => precise.push((prop.to_string(), kept)),
            (Some(i), None) => {
                precise.remove(i);
            }
            (None, None) => {}
        }
        return;
    }
    // the longhands to write again: `prop`'s, and every declaration's that shares one, until none more does
    let mut longhands: Vec<String> = page::style::inline::longhand_set(prop).to_vec();
    let mut again = vec![false; precise.len()];
    loop {
        let mut grew = false;
        for (i, (k, _)) in precise.iter().enumerate() {
            if again[i] {
                continue;
            }
            let theirs = page::style::inline::longhand_set(k);
            if k == prop || theirs.iter().any(|l| longhands.contains(l)) {
                again[i] = true;
                grew = true;
                for l in theirs.iter() {
                    if !longhands.contains(l) {
                        longhands.push(l.clone());
                    }
                }
            }
        }
        if !grew {
            break;
        }
    }
    let mut out: Vec<(String, String)> = std::mem::take(precise).into_iter().zip(again).filter(|(_, a)| !a).map(|(d, _)| d).collect();
    // the block's declarations of those longhands -- and a shorthand the block keeps whole because its value holds a
    // `var()` (its longhands wait on the substitution): written as the longhands alone it was gone, and Framer's
    // `background: linear-gradient(var(..))` then `backgroundColor = ..` computed no image (minea's buttons lost
    // their colour), where Chrome 152 keeps the gradient
    out.extend(split_decls(Some(
        block.css_text_of(|n| page::style::inline::longhand_set(n).iter().any(|l| longhands.contains(l))).as_str(),
    )));
    *precise = out;
    *disjoint = shares_none(precise);
}

/// Does no declaration of `precise` but `prop`'s own (one at most) share a longhand with `prop`?
fn alone(precise: &[(String, String)], prop: &str) -> bool {
    let mine = page::style::inline::longhand_set(prop);
    let mut own = false;
    for (k, _) in precise {
        if k == prop {
            if own {
                return false;
            }
            own = true;
        } else if page::style::inline::longhand_set(k).iter().any(|l| mine.contains(l)) {
            return false;
        }
    }
    true
}

/// Do no two declarations of `precise` share a longhand ([`Slot::disjoint`])?
fn shares_none(precise: &[(String, String)]) -> bool {
    let sets: Vec<std::rc::Rc<[String]>> = precise.iter().map(|(k, _)| page::style::inline::longhand_set(k)).collect();
    sets.iter().enumerate().all(|(i, a)| sets[i + 1..].iter().all(|b| !a.iter().any(|l| b.contains(l))))
}

/// `value` with every block it leaves open closed, as the tokenizer closes them at the end of the value it was
/// given alone (an open string too). A setter's value is parsed alone -- Fingerprint's `calc( 1px * ( sin( ...
/// ) ) )` is one `)` short and valid, Chrome 150 lays it out 0.203125px wide -- but the cascade reads it as one
/// declaration of a list, where the open `calc(` swallowed the `;` after it and the declaration was invalid.
fn closed(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    out.push_str(value);
    close(&mut out, &scan(value, |_| {}));
    out
}

/// Append to `out` what closes `open`, innermost first, reading as the tokenizer read the text at its end: a comment
/// closed (it ran to the end and swallowed the list's next `;`), a final backslash given the character the
/// tokenizer reads it as -- U+FFFD, or in a string nothing (an escaped newline) -- so the `;` after it is not
/// escaped, then the url, the string and the blocks.
fn close(out: &mut String, open: &Open) {
    if open.escape {
        out.push(if open.quote.is_some() { '\n' } else { '\u{FFFD}' });
    }
    if open.comment {
        out.push_str("*/");
    }
    if open.url {
        out.push(')');
    }
    if let Some(q) = open.quote {
        out.push(q as char);
    }
    for &c in open.blocks.iter().rev() {
        out.push(c as char);
    }
}

fn cb_csd_get_property_value(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let prop = args.get(0).to_rust_string_lossy(scope);
    // one look at the declaration's kind ([`csd_kind`])
    match csd_kind(scope, args.this()) {
        Kind::Element(id) => {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let val = { let st = op_state.borrow(); with_inline(&st.borrow::<HostDom>().0, id, |b| b.value(&prop)) };
            let v = str_val(scope, &val); rv.set(v); return;
        }
        Kind::Computed(node) => {
            let s = computed_value(scope, node, &dashed(&prop));
            let v = str_val(scope, &s); rv.set(v); return;
        }
        Kind::Other => {}
    }
    if let Some(s) = rule_block_value(scope, args.this(), &prop) {
        let v = str_val(scope, &s); rv.set(v); return;
    }
    let e = str_val(scope, ""); rv.set(e);
}
/// `setProperty(property, value, priority)`: `priority` is `important` or nothing -- any other string and Blink
/// changes nothing (`AbstractPropertySetCSSStyleDeclaration::setProperty`). This ignored it: Chrome 150 reads
/// `setProperty('padding', '4px', 'important')` back with `getPropertyPriority('padding')` `important`.
///
/// The three arguments are converted first, in order, and a conversion that throws ends the call ([`css_string`]);
/// an omitted or `undefined` priority is its default `""` and a `null` one `""` too (`[LegacyNullToEmptyString]
/// optional ... = ""`: Chrome 152 writes `setProperty('width', '5px', undefined)`, which korvus dropped as the
/// priority "undefined"). A rule's block takes the write as an element's does (`MutableCSSPropertyValueSet::
/// SetProperty`: Chrome 152 reads `rule.style.setProperty('color', 'blue', 'important')` back in the rule's
/// `cssText`; korvus changed nothing).
fn cb_csd_set_property(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let (mut nbuf, mut vbuf, mut pbuf) = ([std::mem::MaybeUninit::<u8>::uninit(); 64], [std::mem::MaybeUninit::<u8>::uninit(); 64], [std::mem::MaybeUninit::<u8>::uninit(); 64]);
    let Some(name) = css_string(scope, args.get(0), false, &mut nbuf) else { return };
    let Some(value) = css_string(scope, args.get(1), true, &mut vbuf) else { return };
    let priority = if args.get(2).is_undefined() {
        std::borrow::Cow::Borrowed("")
    } else {
        let Some(p) = css_string(scope, args.get(2), true, &mut pbuf) else { return };
        p
    };
    let this = args.this();
    let eid = csd_eid(scope, this);
    if eid.is_none() && computed_refuses(scope, this, Refused::Execute("setProperty"), Some(&name)) { return; }
    let important = priority.eq_ignore_ascii_case("important");
    if !important && !priority.is_empty() {
        return;
    }
    let prop = if name.starts_with("--") { name.into_owned() } else { name.to_ascii_lowercase() };
    let Some(id) = eid else {
        if let Some((sheet, path)) = crate::core::css::css_rule::rule_pos(scope, this) {
            rule_block_edit(scope, sheet, &path, |b| rule_set(b, &prop, &value, important));
        }
        return;
    };
    mutate_inline(scope, id, false, |_| false, |dom| inline_write(dom, id, &prop, &value, important));
}

/// A rule's block changed as `f` changes it -- the block read from the rule's text, changed as an element's is
/// (`MutableCSSPropertyValueSet`: a longhand replaced where it stands or appended, a removal of its longhands), and
/// written back ([`rule_block_write`]) only when `f` changed it: a removal of what is not there, a value the setter
/// rejects or the value already standing writes nothing.
///
/// A style rule's, a keyframe's and a nested-declarations rule's block only. A descriptor at-rule's (`@font-face`,
/// `@page`, `@counter-style`, `@position-try`) is left as it was and `f` sees an empty block: its block holds what an
/// element's does not (`src`, `unicode-range`, `font-display`, `size`), and a write of the whole block re-made from
/// an element's parse dropped them all -- Chrome 152 (`vrule.html`, 2026-10-23) keeps `@font-face { font-family: f1;
/// src: local("Arial"); unicode-range: U+0-7F; font-display: swap; }` through `setProperty('font-display', 'swap')`,
/// where this left `@font-face { font-family: f1; }` and `document.fonts.size` went 2 -> 0.
fn rule_block_edit<R>(
    scope: &mut v8::PinScope,
    sheet: v8::Local<v8::Object>,
    path: &[u32],
    f: impl FnOnce(&mut page::style::inline::InlineStyle) -> R,
) -> R {
    use page::style::cssom as codes;
    let handle = crate::core::css::style_sheet::sheet_handle(scope, sheet);
    let kind = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        crate::core::css::cssom::rule_kind(&mut st, handle, path)
    };
    if !matches!(kind, codes::RULE_STYLE | codes::RULE_KEYFRAME | codes::RULE_NESTED_DECLARATIONS) {
        return f(&mut page::style::inline::InlineStyle::default());
    }
    let text = crate::core::css::css_rule::block_text(scope, sheet, path);
    let mut block = page::style::inline::InlineStyle::parse_css_text(&text);
    let before = block.css_text();
    let r = f(&mut block);
    let changed = block.css_text();
    if changed != before {
        rule_block_write(scope, sheet, path, &changed);
    }
    r
}

/// Replace a rule's declaration block with `text`: a style rule's own declarations in place, its selectors and the
/// rules nested in it kept (`cssom::set_style_block`: Chrome 152 keeps `.p { color: green; & .n { ... } }`'s nested
/// rule through `.p`'s `style.padding = '1px'`, `style.setProperty(...)` and `style.cssText = ...`; the rule was
/// re-made from `selector { text }`, which dropped it), any other rule as `css_rule::set_block_text` writes it.
fn rule_block_write(scope: &mut v8::PinScope, sheet: v8::Local<v8::Object>, path: &[u32], text: &str) {
    let handle = crate::core::css::style_sheet::sheet_handle(scope, sheet);
    let done = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        crate::core::css::cssom::set_style_block(&mut st, handle, path, text)
    };
    if !done {
        crate::core::css::css_rule::set_block_text(scope, sheet, path, text);
    }
}

/// One setter's write to a rule's block: the setter's checks ([`accept`]), then the block's own set; an empty value
/// removes the property.
fn rule_set(block: &mut page::style::inline::InlineStyle, prop: &str, value: &str, important: bool) {
    let raw = value.trim();
    if raw.is_empty() {
        block.remove(prop);
        return;
    }
    if let Some(canonical) = accept(prop, raw, important) {
        block.set_outcome(prop, &canonical, important);
    }
}

/// `removeProperty(property)`: the property's value before, `""` for a shorthand (Blink's `RemoveProperty` fills it
/// for a longhand only: Chrome 152 answers `""` for `removeProperty('margin')` and `'border-top'`, korvus their
/// values), and the removal itself on an element's block or a rule's.
fn cb_csd_remove_property(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let mut nbuf = [std::mem::MaybeUninit::<u8>::uninit(); 64];
    let Some(name) = css_string(scope, args.get(0), false, &mut nbuf) else { return };
    let this = args.this();
    let eid = csd_eid(scope, this);
    if eid.is_none() && computed_refuses(scope, this, Refused::Execute("removeProperty"), Some(&name)) { return; }
    let prop = if name.starts_with("--") { name.into_owned() } else { name.to_ascii_lowercase() };
    let old = match eid {
        Some(id) => {
            let mut old = String::new();
            mutate_inline(scope, id, false, |_| false, |dom| {
                old = with_inline(dom, id, |b| b.removal_value(&prop));
                inline_write(dom, id, &prop, "", false);
            });
            old
        }
        None => match crate::core::css::css_rule::rule_pos(scope, this) {
            Some((sheet, path)) => rule_block_edit(scope, sheet, &path, |b| {
                let old = b.removal_value(&prop);
                b.remove(&prop);
                old
            }),
            None => String::new(),
        },
    };
    let v = str_val(scope, &old); rv.set(v);
}
/// `getPropertyPriority`: `important` for an inline declaration set so (a shorthand: all its longhands), "" for
/// everything else. It answered "" always.
fn cb_csd_get_property_priority(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let prop = args.get(0).to_rust_string_lossy(scope);
    let mut answer = "";
    // an element's slot is never on a computed declaration ([`csd_eid`])
    if let Some(id) = csd_eid(scope, args.this()) {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        answer = with_inline(&st.borrow::<HostDom>().0, id, |b| b.priority(&prop));
    } else if let Some((sheet, path)) = crate::core::css::css_rule::rule_pos(scope, args.this()) {
        // a rule's block answers too (Chrome 152 reads `important` after `rule.style.setProperty('width', '6px',
        // 'important')`; this answered "" for every rule)
        let text = crate::core::css::css_rule::block_text(scope, sheet, &path);
        answer = page::style::inline::InlineStyle::parse_css_text(&text).priority(&prop);
    }
    let e = str_val(scope, answer);
    rv.set(e);
}
/// `item(index)`: `unsigned long`, so the index is ToUint32 of the argument -- modulo 2^32 (Chrome 152: `item(4294967296)`
/// is `item(0)`, `item(-4294967295)` `item(1)`; korvus answered ""), and a conversion that throws ends the call.
fn cb_csd_item(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(idx) = args.get(0).uint32_value(scope) else { return };
    let name = decl_name_at(scope, args.this(), idx).unwrap_or_default();
    let v = str_val(scope, &name);
    rv.set(v);
}
fn cb_csd_length(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let n = decl_len(scope, args.this());
    rv.set_uint32(n);
}
/// `cssText`, for all THREE kinds of declaration block this interface has.
///
/// An INLINE block keys on an element id (`csd_eid`) and is the `style` ATTRIBUTE's text. A COMPUTED
/// block has no `cssText` at all, per CSSOM. A RULE's block -- `sheet.cssRules[i].style`, and the
/// `CSSPositionTryDescriptors` object a `@position-try` rule hands out -- has no element, so it used
/// to fall through to the element branch and answer `""`.
///
/// The kernel's `BOOTSTRAP` string papered over that by WRAPPING this accessor: it read the
/// descriptor off `CSSStyleDeclaration.prototype`, kept it in a closure, and re-defined the member as
/// an `op_helper_get`/`op_helper_set` pair over a `KB.__csdCssText*` bridge slot that dispatched on a
/// `WeakMap` of blocks it had minted. That decorator is the reason this table's install was pinned to
/// run BEFORE the bootstrap script -- installed after it, the plain row overwrote the decorator and
/// every at-rule block's `cssText` went back to `""`. The third case is a row's own branch now, the
/// decorator is deleted, and no install order buys anything.
fn cb_csd_css_text_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match csd_kind(scope, args.this()) {
        Kind::Element(id) => {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            // the parsed block serialized (`AsText`), not the attribute's text (a bot-detection script D6)
            let s = { let st = op_state.borrow(); with_inline(&st.borrow::<HostDom>().0, id, |b| b.css_text()) };
            let v = str_val(scope, &s); rv.set(v); return;
        }
        // A computed declaration block has no cssText, per CSSOM.
        Kind::Computed(_) => { let e = str_val(scope, ""); rv.set(e); return; }
        Kind::Other => {}
    }
    if let Some((sheet, path)) = crate::core::css::css_rule::rule_pos(scope, args.this()) {
        let s = crate::core::css::css_rule::block_text(scope, sheet, &path);
        let v = str_val(scope, &s); rv.set(v); return;
    }
    let e = str_val(scope, ""); rv.set(e);
}
fn cb_csd_css_text_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let mut buf = [std::mem::MaybeUninit::<u8>::uninit(); 64];
    let Some(val) = css_string(scope, args.get(0), true, &mut buf) else { return };
    if computed_refuses(scope, args.this(), Refused::Set("cssText"), None) { return; }
    if let Some((sheet, path)) = crate::core::css::css_rule::rule_pos(scope, args.this()) {
        rule_block_write(scope, sheet, &path, &val);
        return;
    }
    let id = match csd_eid(scope, args.this()) { Some(i) => i, None => return };
    // the `style` attribute mutation it is: a record even when the text does not change
    mutate_inline(scope, id, true, |_| false, |dom| inline_css_text(dom, id, &val));
}

/// `cssText = text` on an element's inline block: re-parsed, and only the declarations the properties accept survive
/// (S28) -- then made a block as `ParseDeclarationList` makes one: `!important` last, the last of a property winning.
fn inline_css_text(dom: &mut Dom, id: u32, text: &str) {
    let mut decls: Vec<(String, String)> = Vec::new();
    let mut precise: Vec<(String, String)> = Vec::new();
    for d in declarations(text) {
        let Some(i) = d.find(':') else { continue };
        let k = d[..i].trim();
        let k = if k.starts_with("--") { k.to_string() } else { k.to_ascii_lowercase() };
        if k.is_empty() { continue; }
        css_text_declaration(&mut decls, &mut precise, k, &d[i + 1..]);
    }
    let block = page::style::inline::InlineStyle::parse_css_text(&decls_text(&decls, 0).0);
    let exact = style_write(dom, id, &block, &precise);
    let d = shares_none(&precise);
    keep_slot(dom, id, block, exact.then_some(precise), d);
}
fn cb_csd_css_float_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match csd_kind(scope, args.this()) {
        Kind::Element(id) => {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let val = { let st = op_state.borrow(); with_inline(&st.borrow::<HostDom>().0, id, |b| b.value("float")) };
            let v = str_val(scope, &val); rv.set(v); return;
        }
        Kind::Computed(node) => {
            let s = computed_value(scope, node, "float");
            let v = str_val(scope, &s); rv.set(v); return;
        }
        Kind::Other => {}
    }
    // a rule's block: its `float` (Chrome 152 reads `rule.style.cssFloat` `left` after setting it; this read "")
    if let Some(s) = rule_block_value(scope, args.this(), "float") {
        let v = str_val(scope, &s); rv.set(v); return;
    }
    let e = str_val(scope, ""); rv.set(e);
}
/// `cssFloat`'s setter: `float`'s, on an element's block or a rule's (Chrome 152 reads `rule.style.cssFloat = 'left'`
/// back; korvus changed nothing there).
fn cb_csd_css_float_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let mut buf = [std::mem::MaybeUninit::<u8>::uninit(); 64];
    let Some(value) = css_string(scope, args.get(0), true, &mut buf) else { return };
    let this = args.this();
    let eid = csd_eid(scope, this);
    if eid.is_none() && computed_refuses(scope, this, Refused::Set("cssFloat"), Some("float")) { return; }
    let Some(id) = eid else {
        if let Some((sheet, path)) = crate::core::css::css_rule::rule_pos(scope, this) {
            rule_block_edit(scope, sheet, &path, |b| rule_set(b, "float", &value, false));
        }
        return;
    };
    // the `style` attribute mutation it is, with its reaction run here (`cssFloat` has [CEReactions])
    mutate_inline(scope, id, false, |_| true, |dom| inline_write(dom, id, "float", &value, false));
}
fn cb_csd_parent_rule(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) { let _ = scope; rv.set_null(); }

// ---------------------------------------------------------------------------------------------
// The named-CSS-property interceptor: `el.style.color`.
//
// The ~500 CSS property names are not own members of `CSSStyleDeclaration.prototype` in any browser;
// they are answered by the object itself, which is exactly what a WebIDL named-property mechanism is
// for. Korvus answered them with a JS `Proxy` whose `get` trap did `t[prop].bind(t)` for the five
// prototype METHODS -- because a native method entered with the Proxy as its receiver cannot find the
// element id in the target's private slot. The bind was correct and it was a first-tier tell:
// `el.style.setProperty !== el.style.setProperty` (a fresh bound function per read, which no browser
// produces) and `el.style.setProperty.name === "bound setProperty"`.
//
// An interceptor on the object itself has no such problem: the prototype chain is untouched, so the
// five methods are reached ordinarily with the declaration as `this`, and only names the prototype
// does NOT answer reach the CSS store.
// ---------------------------------------------------------------------------------------------

/// `backgroundColor` -> `background-color`, matching the JS `p.replace(/[A-Z]/g, ...)` it replaces.
/// A RULE's declaration block, read by property: `rule.style.color`,
/// `pageRule.style.margin`, `fontFaceRule.style.getPropertyValue("src")`. `None` when `this` is not a
/// rule's block. These answered `""` for every rule until now; Chrome 150 answers `red`, `1in` and
/// `url("a.woff2")` for `a { color: red }`, `@page :first { margin: 1in }` and `@font-face { src:
/// url(a.woff2) }`.
fn rule_block_value(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, name: &str) -> Option<String> {
    let (sheet, path) = crate::core::css::css_rule::rule_pos(scope, this)?;
    let handle = crate::core::css::style_sheet::sheet_handle(scope, sheet);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    Some(crate::core::css::cssom::rule_property_value(&mut st, handle, &path, name))
}

/// A RULE's declaration block's property names, in order; `None` when `this` is not a rule's block.
fn rule_block_names(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Option<Vec<String>> {
    let (sheet, path) = crate::core::css::css_rule::rule_pos(scope, this)?;
    let handle = crate::core::css::style_sheet::sheet_handle(scope, sheet);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    Some(crate::core::css::cssom::rule_property_names(&mut st, handle, &path))
}

fn dashed(p: &str) -> String {
    let mut out = String::with_capacity(p.len() + 4);
    for c in p.chars() {
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// The descriptor `CSSStyleDeclaration.prototype` owns for this name, if any. The old Proxy consulted
/// exactly this object (`Object.getOwnPropertyDescriptor(Object.getPrototypeOf(t), prop)`), so the set
/// of names that reach the CSS store is unchanged.
fn proto_desc<'s>(scope: &mut v8::PinScope<'s, '_>, holder: v8::Local<v8::Object>, key: v8::Local<v8::Name>) -> Option<v8::Local<'s, v8::Value>> {
    let proto = holder.get_prototype(scope)?;
    let p = v8::Local::<v8::Object>::try_from(proto).ok()?;
    p.get_own_property_descriptor(scope, key).filter(|d| !d.is_undefined())
}

/// Whether the prototype OWNS `key` -- [`proto_desc`]`.is_some()` without building the descriptor
/// object. The prototype is an ordinary object, for which `[[GetOwnProperty]]` returning
/// a descriptor and `HasOwnProperty` answering true are the same fact; the getter and the query
/// handler only ever asked the yes/no, and a read of `getComputedStyle(el).color` allocated a
/// descriptor to get it.
fn proto_owns(scope: &mut v8::PinScope, holder: v8::Local<v8::Object>, key: v8::Local<v8::Name>) -> bool {
    let Some(proto) = holder.get_prototype(scope) else { return false };
    let Ok(p) = v8::Local::<v8::Object>::try_from(proto) else { return false };
    p.has_own_property(scope, key).unwrap_or(false)
}

/// A string key's text, read into the caller's `buf` (no heap copy for a name of up to 64 bytes); `None` for a
/// symbol.
fn named_key<'b>(scope: &mut v8::PinScope, key: v8::Local<v8::Name>, buf: &'b mut [std::mem::MaybeUninit<u8>; 64]) -> Option<std::borrow::Cow<'b, str>> {
    let v: v8::Local<v8::Value> = key.into();
    let s = v8::Local::<v8::String>::try_from(v).ok()?;
    Some(s.to_rust_cow_lossy(scope, buf))
}

/// The names the worn build lists as a declaration's NAMED properties, once per runtime:
/// the JS names in its enumerator's order (`Persona::css_named_properties`, the bundle's
/// `css-named-properties.json`), and the CSS name each stands for -- the set a name must parse into to be one.
struct CssNames {
    js: Vec<String>,
    valid: std::collections::HashSet<String>,
    /// [`style_attribute_property`]'s answers by attribute name: a function of the name and of `valid`, which is
    /// fixed for the runtime. Every `el.style.x` read and write asked it afresh -- two allocations, a SipHash
    /// probe, a binary search and two Strings (2026-10-23). Bounded by clearing.
    resolved: std::cell::RefCell<korvus_dom::FxHashMap<Box<str>, Option<(std::rc::Rc<str>, std::rc::Rc<str>)>>>,
}

struct CssNamesHandle(std::rc::Rc<CssNames>);

/// The CSS property a JS name stands for -- the inverse of Blink's `GetJSPropertyName`: `webkitFoo` ->
/// `-webkit-foo`, `epubFoo` -> `-epub-foo`, otherwise each capital becomes `-` and its lower case.
fn js_to_css(js: &str) -> String {
    let (mut out, rest) = if let Some(r) = js.strip_prefix("webkit") {
        (String::from("-webkit"), r)
    } else if let Some(r) = js.strip_prefix("epub") {
        (String::from("-epub"), r)
    } else {
        (String::new(), js)
    };
    for c in rest.chars() {
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Blink's `GetJSPropertyName` for a CSS name (`-webkit-foo-bar` -> `webkitFooBar`): the engine's own table's
/// names when the persona lists none.
fn css_to_js(css: &str) -> String {
    let body = css.strip_prefix('-').unwrap_or(css);
    let mut out = String::with_capacity(body.len());
    let mut up = false;
    for c in body.chars() {
        if c == '-' {
            up = true;
        } else if up {
            out.push(c.to_ascii_uppercase());
            up = false;
        } else {
            out.push(c);
        }
    }
    out
}

fn css_names(scope: &mut v8::PinScope) -> std::rc::Rc<CssNames> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if let Some(h) = op_state.borrow().try_borrow::<CssNamesHandle>() {
        return h.0.clone();
    }
    let listed = crate::binding::protos::persona_handle(&op_state.borrow()).and_then(|h| h.0.css_named_properties());
    let js: Vec<String> = match listed {
        Some(l) => l.as_ref().clone(),
        None => {
            // No persona list: the engine's own snapshot of Blink's table, JS names sorted by code unit as
            // Blink's enumerator sorts them.
            let mut v: Vec<String> = page::style::blink_names::BLINK_CSS_NAMES.iter().map(|(n, _)| css_to_js(n)).collect();
            v.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            v.dedup();
            v
        }
    };
    let valid = js.iter().map(|j| js_to_css(j)).collect();
    let names = std::rc::Rc::new(CssNames { js, valid, resolved: Default::default() });
    op_state.borrow_mut().put(CssNamesHandle(names.clone()));
    names
}

/// Blink's `ParseCSSPropertyID` (`css_style_declaration.cc`), step for step: a `webkit`/`Webkit` prefix becomes
/// `-webkit-`; any other capitalised name is not one; every capital becomes `-` + its lower case; a name mixing
/// dashes and capitals (`border-rightColor`) is not one. Which names this yields is the whole rule of what a
/// declaration answers as a property: `epubCaptionSide` (an enumerated JS name) parses to `epub-caption-side`,
/// which is no property, so Chrome lists it among the own names and answers no descriptor for it.
fn parse_css_property_id(name: &str) -> Option<String> {
    let first = *name.as_bytes().first()?;
    if !first.is_ascii() {
        return None;
    }
    let mut prop = String::with_capacity(name.len() + 4);
    if name.starts_with("webkit") || name.starts_with("Webkit") {
        prop.push('-');
    } else if first.is_ascii_uppercase() {
        return None;
    }
    let (mut seen_dash, mut seen_upper) = (false, first.is_ascii_uppercase());
    prop.push(first.to_ascii_lowercase() as char);
    for ch in name[1..].chars() {
        if ch.is_ascii_uppercase() {
            seen_upper = true;
            prop.push('-');
            prop.push(ch.to_ascii_lowercase());
        } else {
            seen_dash |= ch == '-';
            prop.push(ch);
        }
    }
    if seen_dash && seen_upper {
        return None;
    }
    Some(prop)
}

/// The CSS property a CSSStyleDeclaration attribute name stands for -- its camel-cased, webkit-cased or dashed
/// attribute (CSSOM "CSSStyleDeclaration attributes") -- as `(the name it parses to, the property it resolves
/// to)`, or `None` when the name is not one. A name is one exactly when Blink's `ParseCSSPropertyID` makes it a
/// name the worn build exposes ([`css_names`]): Chrome 150 answers `undefined` for `style.fooBarBaz` and
/// `'removeAttribute' in style` is false (jQuery's `removeAttr` on amazon). An alias resolves to its
/// target (`-webkit-mask` -> `mask`) through Blink's table.
fn style_attribute_property(scope: &mut v8::PinScope, name: &str) -> Option<(std::rc::Rc<str>, std::rc::Rc<str>)> {
    let names = css_names(scope);
    if let Some(hit) = names.resolved.borrow().get(name) {
        return hit.clone();
    }
    let answer = parse_css_property_id(name).filter(|css| names.valid.contains(css)).map(|css| {
        let resolved: std::rc::Rc<str> = page::style::blink_names::exposed(&css).unwrap_or(&css).into();
        (std::rc::Rc::<str>::from(css), resolved)
    });
    let mut memo = names.resolved.borrow_mut();
    if memo.len() >= 4096 {
        memo.clear();
    }
    memo.insert(name.into(), answer.clone());
    answer
}

fn cb_style_named_get<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue,
) -> v8::Intercepted {
    match style_named_value(scope, key, args.holder()) {
        Some(v) => {
            rv.set(v);
            v8::Intercepted::kYes
        }
        None => v8::Intercepted::kNo,
    }
}

/// What the named getter answers for `key` on `holder`, or `None` when it declines.
fn style_named_value<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    holder: v8::Local<'s, v8::Object>,
) -> Option<v8::Local<'s, v8::Value>> {
    let mut buf = [std::mem::MaybeUninit::<u8>::uninit(); 64];
    let name = named_key(scope, key, &mut buf)?;
    if proto_owns(scope, holder, key) {
        return None; // a real member: length, cssText, setProperty, ...
    }
    let (_, prop) = style_attribute_property(scope, &name)?;
    // one look at the declaration's kind ([`csd_kind`])
    match csd_kind(scope, holder) {
        Kind::Element(id) => {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let val = {
                let st = op_state.borrow();
                with_inline(&st.borrow::<HostDom>().0, id, |b| b.value(&prop))
            };
            return Some(str_val(scope, &val));
        }
        Kind::Computed(node) => {
            // NO DIGIT BRANCH: V8 canonicalises `s[0]` and `s["0"]` to an ARRAY INDEX and routes them to the
            // indexed interceptor ([`cb_style_indexed_get`]); a non-canonical spelling like `s["00"]` is no
            // property name, and Chrome answers `undefined` for it.
            let s = computed_value(scope, node, &prop);
            return Some(str_val(scope, &s));
        }
        Kind::Other => {}
    }
    let s = rule_block_value(scope, holder, &prop)?;
    Some(str_val(scope, &s))
}

fn cb_style_named_set<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    value: v8::Local<'s, v8::Value>,
    args: v8::PropertyCallbackArguments<'s>,
    _rv: v8::ReturnValue<v8::Boolean>,
) -> v8::Intercepted {
    let mut kbuf = [std::mem::MaybeUninit::<u8>::uninit(); 64];
    let Some(name) = named_key(scope, key, &mut kbuf) else { return v8::Intercepted::kNo };
    let holder = args.holder();
    // Not a CSS property: an ordinary expando, exactly as on any other object.
    let Some((css, prop)) = style_attribute_property(scope, &name) else { return v8::Intercepted::kNo };
    // `el.style` is neither computed nor a rule's block ([`csd_eid`]): those two lookups are made only without it
    let eid = csd_eid(scope, holder);
    // A computed declaration refuses the write as Blink's named setter does (`SetPropertyInternal` on a
    // computed declaration): NoModificationAllowedError, naming the attribute and the property it parsed to
    // (Chrome 150: "... named property 'webkitAppearance' ... the '-webkit-appearance' property is read-only.") --
    // once the value is converted: Chrome 152 calls its `Symbol.toPrimitive` first, and throws ITS error when that
    // throws (`c3.html`).
    if eid.is_none() && csd_computed(scope, holder).is_some() {
        let mut vbuf = [std::mem::MaybeUninit::<u8>::uninit(); 64];
        if css_string(scope, value, true, &mut vbuf).is_none() {
            return v8::Intercepted::kYes;
        }
        crate::binding::protos::throw_dom(
            scope,
            &format!("Failed to set a named property '{name}' on 'CSSStyleDeclaration': These styles are computed, and therefore the '{css}' property is read-only."),
            "NoModificationAllowedError",
        );
        return v8::Intercepted::kYes;
    }
    // A prototype accessor WITH a setter (cssText, cssFloat) takes the write; everything else is a
    // CSS property. Same rule the Proxy's `set` trap applied.
    let has_setter = proto_desc(scope, holder, key)
        .and_then(|d| v8::Local::<v8::Object>::try_from(d).ok())
        .and_then(|o| crate::binding::protos::get_prop(scope, o, "set"))
        .is_some_and(|s| s.is_function());
    if has_setter {
        return v8::Intercepted::kNo;
    }
    let mut vbuf = [std::mem::MaybeUninit::<u8>::uninit(); 64];
    // A rule's block: the declaration is appended to the rule's own text, where it wins over an
    // earlier one of the same property. A priority is the setter's own argument here too, never part of the value:
    // Chrome 152 leaves `rule.style.color = 'red !important'` unwritten (`color: blue;` stays); this wrote `color: red
    // !important;` (`f4sup2.html`, 2026-10-23).
    if let Some((sheet, path)) = eid.is_none().then(|| crate::core::css::css_rule::rule_pos(scope, holder)).flatten() {
        if proto_owns(scope, holder, key) {
            return v8::Intercepted::kNo;
        }
        let Some(val) = css_string(scope, value, true, &mut vbuf) else { return v8::Intercepted::kYes };
        if !val.trim().is_empty() && (bang_outside_strings(&val) || !page::style::supports::supports_declaration(&prop, &val)) {
            return v8::Intercepted::kYes;
        }
        let text = crate::core::css::css_rule::block_text(scope, sheet, &path);
        let joined = if text.trim().is_empty() { format!("{prop}: {val};") } else { format!("{text} {prop}: {val};") };
        rule_block_write(scope, sheet, &path, &joined);
        return v8::Intercepted::kYes;
    }
    let Some(id) = eid else { return v8::Intercepted::kNo };
    let Some(val) = css_string(scope, value, true, &mut vbuf) else { return v8::Intercepted::kYes };
    mutate_inline(scope, id, false, |_| true, |dom| inline_write(dom, id, &prop, &val, false));
    v8::Intercepted::kYes
}

/// `Object.getOwnPropertyNames(style)` past the indices: Blink's `NamedPropertyEnumerator` -- the JS name of every
/// CSS property and alias the build exposes, in its order, for EVERY kind of declaration (inline, computed, a
/// rule's block): Chrome 150 lists 744 names after the indices. They used to be a computed declaration's dashed
/// names and nothing at all for the others. V8 asks the query ([`cb_style_named_query`]) for each before
/// `Object.keys` or `for-in` lists it, so a listed name that parses to no property (`epubCaptionSide`) is in
/// `getOwnPropertyNames` and not in `Object.keys` -- as in Chrome (744 own, 735 keys).
fn cb_style_named_enumerate<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    _args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Array>,
) {
    let names = css_names(scope);
    let arr = v8::Array::new(scope, names.js.len() as i32);
    for (i, name) in names.js.iter().enumerate() {
        let v = str_val(scope, name);
        arr.set_index(scope, i as u32, v);
    }
    rv.set(arr);
}

/// The number of declarations a block lists -- `length` -- for any kind.
fn decl_len(scope: &mut v8::PinScope, holder: v8::Local<v8::Object>) -> u32 {
    let kind = csd_kind(scope, holder);
    if let Kind::Element(id) = kind {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        // the block's longhands: `border: 0` is seventeen, as Chrome 150 counts it
        return with_inline(&st.borrow::<HostDom>().0, id, |b| b.len()) as u32;
    }
    if let Kind::Computed(node) = kind {
        return match node {
            Some(id) => {
                // `CSSComputedStyleDeclaration::length` updates style first (its custom properties are counted).
                crate::core::animation::css::flush_style_for(scope, id);
                let op_state = deno_core::JsRuntime::op_state_from(scope);
                let mut st = op_state.borrow_mut();
                crate::core::css::cssom::css_computed_len(&mut st, id)
            }
            None => 0,
        };
    }
    rule_block_names(scope, holder).map_or(0, |names| names.len() as u32)
}

/// The property name at `index` -- what `item(index)` answers -- for any kind, or `None` past the end.
fn decl_name_at(scope: &mut v8::PinScope, holder: v8::Local<v8::Object>, index: u32) -> Option<String> {
    let kind = csd_kind(scope, holder);
    if let Kind::Element(id) = kind {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        return with_inline(&st.borrow::<HostDom>().0, id, |b| b.name_at(index as usize).map(str::to_string));
    }
    if let Kind::Computed(node) = kind {
        let id = node?;
        crate::core::animation::css::flush_style_for(scope, id);
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        let name = crate::core::css::cssom::css_computed_name_at(&mut st, id, index);
        return (!name.is_empty()).then_some(name);
    }
    rule_block_names(scope, holder)?.get(index as usize).cloned()
}

/// `style[0]` -- the property name at that index, the same order `item` walks, for every kind of declaration
/// (Chrome 150: `el.style[0]` is the first property set, `getComputedStyle(el)[0]` is `accent-color`).
///
/// A SEPARATE HANDLER FROM THE NAMED ONE, and it has to be: V8 canonicalises `s[0]` and `s["0"]` to an array
/// index and routes them here. Past the end is `undefined`, not `""` -- what an absent indexed property is, and
/// what Chrome gives for `s[s.length]`.
fn cb_style_indexed_get<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    index: u32,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue,
) -> v8::Intercepted {
    let Some(name) = decl_name_at(scope, args.holder(), index) else { return v8::Intercepted::kNo };
    let v = str_val(scope, &name);
    rv.set(v);
    v8::Intercepted::kYes
}

/// An index below `length` is an own property -- `{writable: false, enumerable: true, configurable: true}`, a
/// WebIDL indexed property with no indexed setter (Chrome 150, every kind) -- so an assignment to it is refused
/// (`style[0] = 'x'` leaves it) and `0 in el.style` is true once a property is set.
fn cb_style_indexed_query<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    index: u32,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Integer>,
) -> v8::Intercepted {
    if index >= decl_len(scope, args.holder()) {
        return v8::Intercepted::kNo;
    }
    let attrs = v8::Integer::new(scope, v8::PropertyAttribute::READ_ONLY.as_u32() as i32);
    rv.set(attrs);
    v8::Intercepted::kYes
}

/// The indices, `0 .. length` -- listed first by `getOwnPropertyNames`, as Chrome lists them (it had none: an
/// inline declaration listed nothing, a computed one its dashed names).
fn cb_style_indexed_enumerate<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Array>,
) {
    let n = decl_len(scope, args.holder());
    let arr = v8::Array::new(scope, n as i32);
    for i in 0..n {
        let v = v8::Integer::new_from_unsigned(scope, i);
        arr.set_index(scope, i, v.into());
    }
    rv.set(arr);
}

/// `delete style[0]` -- a supported index is not deletable (WebIDL legacy platform objects: `false`, a
/// TypeError in strict code); it deleted nothing and answered `true`.
/// An index PAST `length` is refused too: WebIDL gives this interface no indexed setter, so `style[7] = 'x'` creates
/// nothing (Chrome 152 reads `style[7]` back as `undefined`; korvus kept an own property `7`). One below `length` is
/// refused by its read-only attributes ([`cb_style_indexed_query`]). Chrome also throws in strict mode ("Indexed
/// property setter is not supported."), which this V8 does not let an interceptor tell (see `collections.rs`).
fn cb_style_indexed_set<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    index: u32,
    _value: v8::Local<'s, v8::Value>,
    args: v8::PropertyCallbackArguments<'s>,
    _rv: v8::ReturnValue<v8::Boolean>,
) -> v8::Intercepted {
    if index < decl_len(scope, args.holder()) {
        return v8::Intercepted::kNo;
    }
    v8::Intercepted::kYes
}

fn cb_style_indexed_delete<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    index: u32,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Boolean>,
) -> v8::Intercepted {
    if index >= decl_len(scope, args.holder()) {
        return v8::Intercepted::kNo;
    }
    rv.set_bool(false);
    v8::Intercepted::kYes
}

/// A name that parses to a property the build exposes is an own property -- writable, enumerable and
/// configurable (Blink's `NamedPropertyQuery`: every kind of declaration, a computed one too): `'accentColor'
/// in getComputedStyle(el)`, `'background-color' in el.style` and `'WebkitAppearance' in el.style` are true.
fn cb_style_named_query<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Integer>,
) -> v8::Intercepted {
    let mut buf = [std::mem::MaybeUninit::<u8>::uninit(); 64];
    let Some(name) = named_key(scope, key, &mut buf) else { return v8::Intercepted::kNo };
    if proto_owns(scope, args.holder(), key) || style_attribute_property(scope, &name).is_none() {
        return v8::Intercepted::kNo;
    }
    let attrs = v8::Integer::new(scope, v8::PropertyAttribute::NONE.as_u32() as i32);
    rv.set(attrs);
    v8::Intercepted::kYes
}

/// `CSSStyleDeclaration`'s instance template, which every declaration of every kind is minted from: the
/// named-property interceptor with its query and enumerator, and the indexed one (`s[0]`, which the NAMED
/// handler never sees -- see [`cb_style_indexed_get`]) with its query, enumerator and deleter. Applied once per
/// isolate by the builder (`install::instance_shapes`). Every callback answers for every kind
/// of declaration, as Blink's do.
pub(crate) fn configure(it: v8::Local<v8::ObjectTemplate>) {
    TEMPLATE_BUILDS.with(|c| c.set(c.get() + 1));
    let config = v8::NamedPropertyHandlerConfiguration::new()
        .getter_raw(crate::binding::refs::c(icpt!(named_get, "CSSStyleDeclaration", cb_style_named_get).map_fn_to()))
        .setter_raw(crate::binding::refs::c(icpt!(named_set, "CSSStyleDeclaration", cb_style_named_set).map_fn_to()))
        .query_raw(crate::binding::refs::c(icpt!(named_query, "CSSStyleDeclaration", cb_style_named_query).map_fn_to()))
        .enumerator_raw(crate::binding::refs::c(icpt!(named_keys, "CSSStyleDeclaration", cb_style_named_enumerate).map_fn_to()))
        .flags(v8::PropertyHandlerFlags::ONLY_INTERCEPT_STRINGS);
    it.set_indexed_property_handler(
        v8::IndexedPropertyHandlerConfiguration::new()
            .getter_raw(crate::binding::refs::c(icpt!(idx_get, "CSSStyleDeclaration", cb_style_indexed_get).map_fn_to()))
            .setter_raw(crate::binding::refs::c(icpt!(idx_set, "CSSStyleDeclaration", cb_style_indexed_set).map_fn_to()))
            .query_raw(crate::binding::refs::c(icpt!(idx_query, "CSSStyleDeclaration", cb_style_indexed_query).map_fn_to()))
            .enumerator_raw(crate::binding::refs::c(icpt!(idx_keys, "CSSStyleDeclaration", cb_style_indexed_enumerate).map_fn_to()))
            .deleter_raw(crate::binding::refs::c(icpt!(idx_delete, "CSSStyleDeclaration", cb_style_indexed_delete).map_fn_to())),
    );
    it.set_named_property_handler(config);
}

/// A bare `CSSStyleDeclaration` platform object: an instance of the interface's template ([`configure`]),
/// on `proto`. The caller fills in which KIND it is by setting either the element-id slot (inline) or the
/// computed slot.
fn new_decl<'s>(scope: &mut v8::PinScope<'s, '_>, proto: v8::Local<'s, v8::Object>) -> (v8::Local<'s, v8::Object>, CsdLocal<'s>) {
    let c = cache(scope);
    let decl = crate::binding::protos::mint_on(scope, proto.into());
    (decl, c)
}

/// A rule's declaration block: the same template `el.style` is minted from, so its CSS
/// properties read and write through the named interceptor rather than coming back `undefined`.
pub(crate) fn new_rule_block<'s>(scope: &mut v8::PinScope<'s, '_>, proto: v8::Local<'s, v8::Object>) -> v8::Local<'s, v8::Object> {
    new_decl(scope, proto).0
}

/// `el.style` -- the inline declaration for element `id`.
pub(crate) fn make_style_decl<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32, proto: v8::Local<'s, v8::Object>) -> v8::Local<'s, v8::Object> {
    let (decl, c) = new_decl(scope, proto);
    let idv = v8::Number::new(scope, f64::from(id));
    decl.set_private(scope, c.kind, idv.into());
    decl
}

/// The realm's `CSSStyleDeclaration.prototype`, remembered with the context it belongs to.
struct CsdProto(v8::Global<v8::Context>, v8::Global<v8::Object>);

/// `CSSStyleDeclaration.prototype` for a computed declaration: the SEALED one, looked up once per
/// context.
///
/// It was `globalThis.CSSStyleDeclaration.prototype`, read by name on every `getComputedStyle` call
/// -- ~350 ns of a call Chrome makes in ~250, and the exact defect closed for every other
/// mint site: `globalThis.CSSStyleDeclaration = function Hijacked(){}` made the next call hand back
/// an object wearing the PAGE's prototype, and deleting the global made it a plain `Object`.
/// [`crate::binding::protos::shell_proto`] reads the map sealed at boot, which is what an engine that
/// does not resolve its own prototypes through script-writable names answers.
///
/// Kept beside the context it came from, and used only in that context: a prototype is per realm.
fn computed_proto<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    let ctx = scope.get_current_context();
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    {
        let st = op_state.borrow();
        if let Some(CsdProto(c, p)) = st.try_borrow::<CsdProto>() {
            if v8::Local::new(scope, c) == ctx {
                return Some(v8::Local::new(scope, p));
            }
        }
    }
    let proto = crate::binding::protos::shell_proto(scope, "CSSStyleDeclaration")?;
    let cached = CsdProto(v8::Global::new(scope, ctx), v8::Global::new(scope, proto));
    op_state.borrow_mut().put(cached);
    Some(proto)
}

/// `getComputedStyle(el)` -- a real `CSSStyleDeclaration` that resolves `node` on access.
///
/// `node` is the element to resolve against, or `None` for the always-empty kind: a pseudo-element
/// this engine generates no box for (design D12), or a receiver with no node id. It used to take the
/// resolved `(names, values)` pair, materialised by `cssom::css_computed_all` before this function
/// was reached; see [`csd_computed`] for why carrying the element instead is both faster and closer
/// to Chrome.
///
/// This was `op_computed_style_decl`, and the op is DELETED. Its only caller was
/// `host.js`'s `getComputedStyle`, which is `cssom::cb_get_computed_style` now and calls this as
/// plain Rust -- an op whose only purpose was to be reachable from JS does not outlive the JS.
pub(crate) fn computed_style_decl<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    node: Option<u32>,
) -> v8::Local<'s, v8::Object> {
    let proto = computed_proto(scope);
    let (decl, key) = match proto {
        Some(p) => {
            let (d, c) = new_decl(scope, p);
            (d, c.kind)
        }
        None => (v8::Object::new(scope), cache(scope).kind),
    };
    // The slot is set EITHER WAY: `null` is the always-empty kind, a negative number the element a computed one
    // resolves against ([`csd_kind`]).
    let marker: v8::Local<v8::Value> = match node {
        Some(id) => v8::Number::new(scope, -f64::from(id) - 1.0).into(),
        None => v8::null(scope).into(),
    };
    decl.set_private(scope, key, marker);
    decl
}

// THIS FILE DECLARES NO OPS. `op_computed_style_decl` was its only one and its only
// caller was `host.js`'s `getComputedStyle`; both are gone, and `computed_style_decl` above is an
// ordinary Rust function the `Window` table's row calls.

// ── moved out of bindings.rs (STEP 3 dissolution) ──
/// A declaration's kind and the element it is for ([`csd_kind`]): `el.style`'s element id, a computed declaration's
/// element as `-(id + 1)` -- or `null` for the kind that answers nothing (a pseudo-element this engine generates no
/// box for) -- and no slot at all on a rule's block. It replaced two slots, `korvus_csd_eid` and
/// `korvus_csd_computed`, whose presence told the kinds apart.
pub(crate) const CSD_KIND: &str = "korvus_csd_kind";
/// The element wrapper's slot holding its one `CSSStyleDeclaration` (`el.style === el.style`).
pub(crate) const CSD_CACHE: &str = "korvus_el_style";

/// The body every row of [`CSSSTYLEDECLARATION`] names.
pub fn install_css_style_declaration(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static crate::install::MemberDef) {
    use crate::install::live::{getter_row, method_row, pair_row};
    match member.name {
        "cssText" => pair_row(scope, proto, member, cb_csd_css_text_get, cb_csd_css_text_set),
        "length" => getter_row(scope, proto, member, cb_csd_length),
        "parentRule" => getter_row(scope, proto, member, cb_csd_parent_rule),
        "cssFloat" => pair_row(scope, proto, member, cb_csd_css_float_get, cb_csd_css_float_set),
        "getPropertyPriority" => method_row(scope, proto, member, cb_csd_get_property_priority),
        "getPropertyValue" => method_row(scope, proto, member, cb_csd_get_property_value),
        "item" => method_row(scope, proto, member, cb_csd_item),
        "removeProperty" => method_row(scope, proto, member, cb_csd_remove_property),
        "setProperty" => method_row(scope, proto, member, cb_csd_set_property),
        other => debug_assert!(false, "install_css_style_declaration: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `CSSStyleDeclaration` block of `install/generated_registry.rs`, in the
// capture's order -- cssText, length, parentRule, cssFloat, then the five methods alphabetically.
// `js/dom/08h-abort-attrs.js` installed the same nine members in a DIFFERENT order (its own JS
// interface object's prototype, methods first), so this cutover also puts the enumeration order right.
//
// `[interface, window]`: `surface.json` records `newable: false` and `worker.surface.json` has no
// `CSSStyleDeclaration` at all. Unlike the other nine interfaces of this lane there is no JS class
// behind it -- the interface object AND the prototype are `installer::create_interface`'s, and
// `make_style_decl` / [`computed_style_decl`] mint every declaration against that prototype (the
// second was the op `op_computed_style_decl` until it was deleted with its only caller; both
// read it off the global at call time, so they follow this install with no further wiring).
webapi::interface! {
    "CSSStyleDeclaration" [interface, window] {
        ctor   "CSSStyleDeclaration"                len 0 = stub,
        attr   "cssText"             : Object             = install_css_style_declaration,
        attr   "length"              : Object readonly    = install_css_style_declaration,
        attr   "parentRule"          : Object readonly nullable = install_css_style_declaration,
        attr   "cssFloat"            : Object             = install_css_style_declaration,
        method "getPropertyPriority" : Object len 1 required 1  = install_css_style_declaration,
        method "getPropertyValue"    : Object len 1 required 1  = install_css_style_declaration,
        method "item"                : Object len 1 required 1  = install_css_style_declaration,
        method "removeProperty"      : Object len 1 required 1  = install_css_style_declaration,
        method "setProperty"         : Object len 2 required 2  = install_css_style_declaration,
        method "@@iterator" : Object len 0 hidden cfg "values" = array_proto_fn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Chrome 152 (`c2.html`, 2026-10-23): an element reads its own block, a clone its original's (Blink's
    /// `CloneAttributesFrom` copies `inline_style_`), and an element given the same text by `setAttribute` the
    /// parse -- whichever other element's write left that text last.
    #[test]
    fn a_clone_reads_its_originals_block_and_set_attribute_the_parse() {
        let mut dom = Dom::parse("<html><body></body></html>");
        let (x, y, z) = (dom.create_element("div"), dom.create_element("div"), dom.create_element("div"));
        for side in ["left", "top", "right", "bottom"] {
            inline_write(&mut dom, x, &format!("margin-{side}"), "1px", false);
        }
        for side in ["top", "right", "bottom", "left"] {
            inline_write(&mut dom, y, &format!("margin-{side}"), "1px", false);
        }
        // a write that changes nothing, then one there and back: X's block files under X's own text
        inline_write(&mut dom, x, "margin-top", "1px", false);
        inline_write(&mut dom, x, "margin-top", "2px", false);
        inline_write(&mut dom, x, "margin-top", "1px", false);
        let order = |dom: &Dom, id: u32| with_inline(dom, id, |b| b.names().join(","));
        assert_eq!(dom.get_attribute(x, "style"), Some("margin: 1px;"));
        assert_eq!(order(&dom, x), "margin-left,margin-top,margin-right,margin-bottom");
        assert_eq!(order(&dom, y), "margin-top,margin-right,margin-bottom,margin-left");
        dom.set_attribute(z, "style", "margin: 1px;");
        assert_eq!(order(&dom, z), "margin-top,margin-right,margin-bottom,margin-left", "setAttribute: the parse");
        let cx = dom.clone_node(x, false);
        assert_eq!(order(&dom, cx), "margin-left,margin-top,margin-right,margin-bottom", "a clone of X: X's");
        let cy = dom.clone_node(y, false);
        assert_eq!(order(&dom, cy), "margin-top,margin-right,margin-bottom,margin-left", "a clone of Y: Y's");
        // the clone's own write starts from its original's block
        inline_write(&mut dom, cx, "margin-top", "3px", false);
        assert_eq!(order(&dom, cx), "margin-left,margin-top,margin-right,margin-bottom");
        assert_eq!(dom.get_attribute(cx, "style"), Some("margin: 3px 1px 1px;"));
    }

    /// The order is the element's own whatever the texts are (Chrome 152, `c2rev.html` / `c2rev2.html` / `vc2.html` /
    /// `vc2b.html`, 2026-10-23): a write whose cascade text IS its attribute (values already canonical, an
    /// `!important` first), two elements leaving the same cascade text in two orders, an element whose slot was
    /// dropped, and the attribute set again to the text it held.
    #[test]
    fn the_order_is_the_elements_own_whatever_its_texts() {
        let mut dom = Dom::parse("<html><body></body></html>");
        let order = |dom: &Dom, id: u32| with_inline(dom, id, |b| b.names().join(","));
        let x = dom.create_element("div");
        inline_write(&mut dom, x, "display", "none", true);
        inline_write(&mut dom, x, "width", "10px", false);
        inline_write(&mut dom, x, "height", "10px", false);
        assert_eq!(dom.get_attribute(x, "style"), Some("display: none !important; width: 10px; height: 10px;"));
        let cx = dom.clone_node(x, false);
        assert_eq!(order(&dom, cx), "display,width,height", "the parse files `!important` last");
        // `marginLeft` then `margin`, beside `cssText`'s parse of the same declarations: one cascade text, two orders
        let (a, b) = (dom.create_element("div"), dom.create_element("div"));
        inline_write(&mut dom, a, "margin-left", "1px", false);
        inline_write(&mut dom, a, "margin", "1px", false);
        for (k, v) in [("margin-top", "1px"), ("margin-right", "1px"), ("margin-bottom", "1px"), ("margin-left", "1px")] {
            inline_write(&mut dom, b, k, v, false);
        }
        assert_eq!(dom.get_attribute(a, "style"), dom.get_attribute(b, "style"));
        let (ca, cb) = (dom.clone_node(a, false), dom.clone_node(b, false));
        assert_eq!(order(&dom, ca), "margin-left,margin-top,margin-right,margin-bottom");
        assert_eq!(order(&dom, cb), "margin-top,margin-right,margin-bottom,margin-left");
        // the attribute set to the text it holds -- any number of times -- keeps the block, a clone made then copies it,
        // and the next write builds on it; another text and back reads the parse (Chrome 152, `rv3a.html`,
        // `f4same.html`)
        let text = dom.get_attribute(x, "style").unwrap().to_string();
        dom.set_attribute(x, "style", &text);
        assert_eq!(order(&dom, x), "display,width,height");
        dom.set_attribute(x, "style", &text);
        dom.set_attribute(x, "style", &text);
        assert_eq!(order(&dom, x), "display,width,height");
        let cx = dom.clone_node(x, false);
        assert_eq!(order(&dom, cx), "display,width,height");
        inline_write(&mut dom, x, "width", "11px", false);
        assert_eq!(order(&dom, x), "display,width,height");
        let cx = dom.clone_node(x, false);
        assert_eq!(order(&dom, cx), "display,width,height");
        dom.set_attribute(cx, "style", "width: 1px;");
        dom.set_attribute(cx, "style", "display: none !important; width: 11px; height: 10px;");
        assert_eq!(order(&dom, cx), "width,height,display");
        // an element whose slot is gone reads its own order off its cascade text
        BLOCKS.with(|s| s.borrow_mut().clear());
        assert_eq!(order(&dom, a), "margin-left,margin-top,margin-right,margin-bottom");
        assert_eq!(order(&dom, x), "display,width,height");
    }

    /// An attribute too large to share by its text (`inline::SHARED_BYTES`) is its element's own to keep: elements
    /// read in turn each keep theirs (`vrr.html`, 2026-10-23: forty 30 KB attributes parsed again on almost every
    /// read), a write of another text ends it, and the slots stay within their bytes.
    #[test]
    fn a_large_attribute_is_its_elements_own() {
        let mut dom = Dom::parse("<html><body></body></html>");
        let big = |j: usize| format!("background-image: url(data:image/png;base64,{j}{}); margin: 1px", "A".repeat(30_000));
        let es: Vec<u32> = (0..40)
            .map(|j| {
                let e = dom.create_element("div");
                dom.set_attribute(e, "style", &big(j));
                e
            })
            .collect();
        for _ in 0..2 {
            for &e in &es {
                assert_eq!(with_inline(&dom, e, |b| b.value("margin-top")), "1px");
            }
        }
        let held = |dom: &Dom, e: u32| BLOCKS.with(|b| b.borrow().get(&(dom.instance(), e)).is_some_and(|s| s.holds(dom, e)));
        assert!(es.iter().all(|&e| held(&dom, e)), "each element keeps its own block");
        dom.set_attribute(es[0], "style", &big(99).replace("margin: 1px", "margin: 2px"));
        assert!(!held(&dom, es[0]));
        assert_eq!(with_inline(&dom, es[0], |b| b.value("margin-top")), "2px");
        // a small one is shared by its text, and kept by no element
        let small = dom.create_element("div");
        dom.set_attribute(small, "style", "margin: 3px");
        assert_eq!(with_inline(&dom, small, |b| b.value("margin-top")), "3px");
        assert!(!held(&dom, small));
        for j in 0..40 {
            let e = dom.create_element("div");
            dom.set_attribute(e, "style", &format!("--v: {j}{}", "B".repeat(1 << 20)));
            assert_eq!(with_inline(&dom, e, |b| b.len()), 1);
            assert!(BLOCKS_BYTES.with(|n| n.get()) <= page::style::memo::ELEMENT_BYTES);
        }
    }

    /// A shorthand whose value holds a `var()` stays in the cascade's text when a longhand of it is written after it:
    /// Framer writes `background: linear-gradient(0deg, var(--token, red) 0%, ...)` then `backgroundColor`, and Chrome 152
    /// draws the gradient; with the shorthand rewritten as its (still unsubstituted) longhands, it was gone.
    #[test]
    fn a_var_shorthand_survives_a_later_longhand() {
        let mut dom = Dom::parse("<html><body></body></html>");
        let e = dom.create_element("div");
        inline_write(&mut dom, e, "--border-color", "rgba(0, 0, 0, 0)", false);
        inline_write(&mut dom, e, "background", "linear-gradient(0deg, var(--x, rgb(255, 0, 0)) 0%, blue 100%)", false);
        inline_write(&mut dom, e, "background-color", "rgba(0, 0, 0, 0)", false);
        inline_write(&mut dom, e, "transform", "none", false);
        let text = dom.style_for_cascade(e).unwrap_or("").to_string();
        let shorthand = text.find("background: linear-gradient(0deg, var(--x").unwrap_or_else(|| panic!("{text}"));
        let color = text.find("background-color: rgba(0, 0, 0, 0)").unwrap_or_else(|| panic!("{text}"));
        assert!(shorthand < color, "the longhand written after it still wins: {text}");
    }

    /// The cascade's text reads what the block reads where declarations share a longhand: the last of each longhand
    /// written wins, a shorthand over a longhand written before it and a longhand over a shorthand (Chrome 152,
    /// `rv3c.html`, `f4css.html`, `f4prec.html`, 2026-10-23), and the rest keep the precision the page wrote.
    #[test]
    fn the_cascade_reads_the_last_declaration_of_each_longhand() {
        let mut dom = Dom::parse("<html><body></body></html>");
        // the cascade's own reading of its text: the last declaration of a longhand, an `!important` one first
        let cascade = |dom: &Dom, id: u32, p: &str| {
            let block = page::style::inline::InlineStyle::parse_css_text(dom.style_for_cascade(id).unwrap_or(""));
            (block.value(p), block.priority(p))
        };
        let sets = |dom: &mut Dom, writes: &[(&str, &str, bool)]| {
            let e = dom.create_element("div");
            for &(k, v, important) in writes {
                inline_write(dom, e, k, v, important);
            }
            e
        };
        let e = sets(&mut dom, &[("margin-left", "7px", false), ("margin", "3px", false), ("margin-left", "9px", false)]);
        assert_eq!(cascade(&dom, e, "margin-left"), ("9px".into(), ""));
        assert_eq!(dom.get_attribute(e, "style"), Some("margin: 3px 3px 3px 9px;"));
        let e = sets(&mut dom, &[("margin", "3px", false), ("margin-left", "9px", false), ("margin", "5px", false)]);
        assert_eq!(cascade(&dom, e, "margin-left"), ("5px".into(), ""));
        let e = sets(&mut dom, &[("margin-left", "9px", true), ("margin", "3px", false)]);
        assert_eq!(cascade(&dom, e, "margin-left"), ("3px".into(), ""));
        let e = sets(&mut dom, &[("margin", "3px", true), ("margin-left", "9px", false)]);
        assert_eq!((cascade(&dom, e, "margin-left"), cascade(&dom, e, "margin-top")), (("9px".into(), ""), ("3px".into(), "important")));
        let e = sets(&mut dom, &[("border", "3px solid red", false), ("border-top-width", "9px", false), ("border", "5px solid blue", false)]);
        assert_eq!(cascade(&dom, e, "border-top-width"), ("5px".into(), ""));
        // a style attribute that declared both, then a write
        let e = dom.create_element("div");
        dom.set_attribute(e, "style", "margin-left: 7px; margin: 3px");
        inline_write(&mut dom, e, "margin-left", "9px", false);
        assert_eq!(cascade(&dom, e, "margin-left"), ("9px".into(), ""));
        // the rest keep the precision the page wrote; a write sharing no longhand changes its own declaration alone
        let e = sets(&mut dom, &[("transform", "scale(1.000998)", false), ("margin-left", "7px", false), ("margin", "3px", false)]);
        inline_write(&mut dom, e, "margin-left", "", false);
        assert!(dom.style_for_cascade(e).unwrap().contains("scale(1.000998)"), "{:?}", dom.style_for_cascade(e));
        assert_eq!(cascade(&dom, e, "margin-left"), ("".into(), ""));
        let e = sets(&mut dom, &[("width", "1px", false), ("height", "2px", false), ("width", "3px", false)]);
        assert_eq!(dom.style_for_cascade(e), Some("width: 3px; height: 2px;"));
        // `cssText`: every declaration the properties take, filtered once
        for (text, p, want, attribute) in [
            ("margin-left: 7px; margin: 3px; margin-left: 9px", "margin-left", ("9px", ""), "margin: 3px 3px 3px 9px;"),
            ("margin: 3px; margin-left: 9px; margin: 5px", "margin-left", ("5px", ""), "margin: 5px;"),
            ("width: 1px !important; width: 2px", "width", ("1px", "important"), "width: 1px !important;"),
            ("width: 1px; width: ", "width", ("1px", ""), "width: 1px;"),
            ("width: 1px; width: nope", "width", ("1px", ""), "width: 1px;"),
            ("width: 1px; height: 2px; width: 3px", "width", ("3px", ""), "height: 2px; width: 3px;"),
        ] {
            let e = dom.create_element("div");
            inline_css_text(&mut dom, e, text);
            assert_eq!(dom.get_attribute(e, "style"), Some(attribute), "{text}");
            assert_eq!(cascade(&dom, e, p), (want.0.to_string(), want.1), "{text}");
        }
    }

    /// The cut the cascade's parser makes (`InlineStyle`'s attribute parse): cssparser up to each top-level `;`.
    fn tokenizer_cut(text: &str) -> Vec<&str> {
        let mut input = cssparser::ParserInput::new(text);
        let mut p = cssparser::Parser::new(&mut input);
        let mut out = Vec::new();
        loop {
            let start = p.position();
            let _ = p.parse_until_before::<_, (), ()>(cssparser::Delimiter::Semicolon, |p| {
                while p.next_including_whitespace_and_comments().is_ok() {}
                Ok(())
            });
            out.push(p.slice_from(start));
            if p.next_including_whitespace_and_comments().is_err() {
                return out;
            }
        }
    }

    const TEXTS: &[&str] = &[
        "",
        ";",
        "width: 1px; height: 2px;",
        "background-image:url(/rvp/img;v=1.png); height: 10px",
        "background-image: url(data:image/png;base64,AAA); width: 1px",
        "background-image: URL( data:image/png;base64,AAA ); width: 1px",
        "background-image: url(\"data:image/png;base64,AAA\"); width: 1px",
        "background: url('a;b') no-repeat, url(c;d); color: red",
        "content: \"a;b\"; width: 1px",
        "content: 'a;\"b'; width: 1px",
        "content: \"a\\\";b\"; width: 1px",
        "content: \"a\nb\"; width: 1px; height: 2px",
        "content: \"a\r\nb\"; width: 1px",
        "content: \"a\\\nb;c\"; width: 1px",
        "content: \"a\\\r\nb;c\"; width: 1px",
        "--x: {a;b}; width: 1px",
        "--x: [a;b]; width: 1px",
        "width: calc(1px; 2px); height: 1px",
        "width: calc(1px ] ; 2px); height: 1px",
        "width: 1px /* ; */; height: 2px",
        "width: 1px /* ; height: 2px",
        "a: url(a\"b); width: 1px",
        "a: url(a\"b)\"x); width: 1px",
        "a: url(a(b);c); width: 1px",
        "a: url(a/*b); width: 1px",
        "a: url(a\\);b); width: 1px",
        "a: url(a b;c); width: 1px",
        "a: xurl(a;b); width: 1px",
        "a: -url(a;b); width: 1px",
        "a: 1url(a;b); width: 1px",
        "a: #url(a;b); width: 1px",
        "a: .url(a;b); width: 1px",
        "a: \"x\"url(a\"b); width: 1px",
        "font-family: a\\; width: 1px",
        "font-family: a\\;b; width: 1px",
        "\"a: b; c: d",
        "(a: b; c: d); e: f",
        "a: b) ; c: d",
        "a: } ; c: d",
        "a: é;b: \"é;\"; c: url(é;)",
    ];

    #[test]
    fn declarations_are_cut_where_the_tokenizer_cuts_them() {
        for t in TEXTS {
            assert_eq!(declarations(t), tokenizer_cut(t), "{t:?}");
        }
    }

    /// The markup and cssText cases of a `;` inside a url or a string: one declaration each, the value whole.
    #[test]
    fn a_semicolon_in_a_url_or_a_string_stays_in_its_value() {
        assert_eq!(
            split_decls(Some("background-image:url(/rvp/img;v=1.png); height: 10px")),
            [("background-image".to_string(), "url(/rvp/img;v=1.png)".to_string()), ("height".to_string(), "10px".to_string())]
        );
        assert_eq!(
            split_decls(Some("background-image: url(\"data:image/png;base64,AAA\"); content: \"a;b\"")),
            [("background-image".to_string(), "url(\"data:image/png;base64,AAA\")".to_string()), ("content".to_string(), "\"a;b\"".to_string())]
        );
    }

    /// A closed value ends where the list's `;` after it is, as the tokenizer reads the list -- nothing open swallows
    /// it -- and reads back as the value did alone.
    #[test]
    fn a_closed_value_ends_at_its_semicolon() {
        for v in [
            "10px",
            "calc( 1px * ( sin( 0.5 ) )",
            "url(data:image/png;base64,AAA",
            "url(\"a.png\"",
            "url( a\"b",
            "url(a(b",
            "\"a;b",
            "'a",
            "\"a\nb",
            "\"a\\",
            "a\\",
            "url(a\\",
            "1px /* x",
            "1px /* x *",
            "[a",
            "{a;b",
            "é\\",
        ] {
            let c = closed(v);
            let list = format!("a: {c}; b: c");
            assert_eq!(tokenizer_cut(&list), [format!("a: {c}").as_str(), " b: c"], "{v:?} closed as {c:?}");
            assert_eq!(closed(&c), c, "{v:?}: closing twice adds nothing");
        }
        assert_eq!(closed("1px /* x"), "1px /* x*/");
        assert_eq!(closed("a\\"), "a\\\u{FFFD}");
        assert_eq!(closed("\"a\\"), "\"a\\\n\"");
        assert_eq!(closed("url(a\"b"), "url(a\"b)");
        assert_eq!(closed("url(\"a\""), "url(\"a\")");
        assert_eq!(closed("calc(1px"), "calc(1px)");
    }

    /// [`decls_text`] writes a list that splits back into as many declarations, and into exactly the list when it
    /// says so -- the [`Slot`] keeps the list only then.
    #[test]
    fn decls_text_is_exact_when_it_says_so() {
        let lists: Vec<Vec<(&str, &str)>> = vec![
            vec![("width", "1px"), ("height", "2px")],
            vec![("background-image", "url(data:image/png;base64,AAA)"), ("width", "1px")],
            vec![("background-image", "url(\"data:image/png;base64,AAA\")"), ("content", "\"a;b\"")],
            vec![("content", "\"a\nb\"; width: 1px"), ("height", "2px")],
            vec![("background", "url(a\"b)"), ("width", "1px")],
            vec![("width", "1px /* x"), ("height", "2px")],
            vec![("font-family", "a\\"), ("width", "1px")],
            vec![("\"a", "b; c"), ("width", "1px")],
            vec![("(a", "b"), ("width", "1px")],
            vec![("transform", "scale(1.0009981)"), ("--x", "{a;b}")],
            vec![("width", "calc(1px")],
        ];
        for l in lists {
            let d: Vec<(String, String)> = l.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            let (text, exact) = decls_text(&d, 0);
            let back = split_decls(Some(&text));
            assert_eq!(back.len(), d.len(), "{d:?} wrote {text:?}");
            assert_eq!(declarations(&text), tokenizer_cut(&text), "{text:?}");
            if exact {
                assert_eq!(back, d, "{text:?}");
            }
        }
        let exact = |l: &[(&str, &str)]| decls_text(&l.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>(), 0).1;
        assert!(exact(&[("background-image", "url(data:image/png;base64,AAA)"), ("width", "1px")]));
        assert!(exact(&[("content", "\"a;b\""), ("width", "1px")]));
        assert!(!exact(&[("width", "1px /* x")]));
        assert!(!exact(&[("width", "calc(1px")]));
    }
}
