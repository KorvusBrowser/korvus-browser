//! `var()` and `env()` substitution.
//!
//! # Why this is in v1
//!
//! 28.8% of the declarations in a corpus of real stylesheets (MDN, GitHub, Wikipedia, Hacker News)
//! are written `color: var(--color-text-primary)`. They parse, they cascade, and without this they
//! compute to that *string*. `getComputedStyle(el).color === "var(--color-text-primary)"` is as
//! plain a tell as the zero `getBoundingClientRect` this goal exists to close. See
//! `tests/real_css.rs` for the measurement.
//!
//! # Substitution happens late, and that is the whole design
//!
//! A declaration containing `var()` cannot be understood at parse time: `margin: var(--gap)` might
//! be one value or four, and `color: var(--x)` might not be a colour at all. So such a declaration
//! is kept as *text* — not expanded, not validated — until the cascade knows the element's custom
//! properties. Only then is it substituted and re-parsed. CSS calls a declaration that fails at
//! that point "invalid at computed-value time", and it is the reason this cannot simply run
//! earlier.

/// The most `var()` references to follow through one another before giving up.
///
/// A cycle (`--a: var(--b); --b: var(--a)`) makes every declaration in it invalid, which is what
/// running out of depth produces here. Sixteen is far past anything a real stylesheet nests and far
/// short of a stack problem.
const MAX_DEPTH: usize = 16;

/// What a value's substitution functions -- `var()` and `env()` -- make of its declaration at parse time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum References {
    /// None: the value is parsed as it is.
    None,
    /// At least one: the declaration waits, as text, for the element's custom properties.
    Some,
    /// A reference that is not one -- `var(x)`, `var()`, `var(--x 1)`, `env()`, `env(, 6px)`, `env(nope -1, 3px)`:
    /// Chrome 150 drops the declaration, a custom property's included.
    Invalid,
}

/// Does this value contain a `var()` or an `env()` reference?
///
/// `env()` defers a declaration exactly as `var()` does (Blink takes both for its variable references):
/// `padding: 1px env(safe-area-inset-right) 2px` has no arity until it is substituted. Chrome 150 keeps it
/// as written and applies it; parsed now, the shorthand was dropped, and a longhand's `max(22px,
/// env(safe-area-inset-left))` computed 0 -- apple.com's locale banner lost its 22 px side padding.
///
/// Tokenized rather than searched, because `content: "var(--x)"` is a string containing those
/// characters and not a reference — treating it as one would defer a declaration that is perfectly
/// resolvable now.
pub fn references(value: &str) -> References {
    let mut input = cssparser::ParserInput::new(value);
    let mut parser = cssparser::Parser::new(&mut input);
    let mut found = References::None;
    scan_for_references(&mut parser, &mut found);
    found
}

/// Walk every token, descending into blocks. It keeps walking after a hit rather than returning:
/// cssparser requires a nested block to be consumed entirely, and bailing out of one mid-way makes
/// `parse_nested_block` report an error that would be read here as "no `var()` found".
fn scan_for_references(parser: &mut cssparser::Parser, found: &mut References) {
    use cssparser::Token;
    loop {
        let (opens_block, var, env) = match parser.next() {
            Err(_) => return,
            Ok(Token::Function(name)) => {
                let (var, env) = (name.eq_ignore_ascii_case("var"), name.eq_ignore_ascii_case("env"));
                if (var || env) && *found == References::None {
                    *found = References::Some;
                }
                (true, var, env)
            }
            Ok(Token::ParenthesisBlock | Token::SquareBracketBlock | Token::CurlyBracketBlock) => (true, false, false),
            Ok(_) => (false, false, false),
        };
        if opens_block {
            let _ = parser.parse_nested_block(|inner| -> Result<(), cssparser::ParseError<'_, ()>> {
                if (var || env) && !reference_head(inner, env) {
                    *found = References::Invalid;
                }
                scan_for_references(inner, found);
                Ok(())
            });
        }
    }
}

/// Is what opens a `var()` -- a custom property's name -- or an `env()` -- a name and its non-negative integer
/// indices -- followed by the end or the comma before a fallback? It reads up to that comma; the fallback is left
/// to be scanned for references of its own.
fn reference_head(inner: &mut cssparser::Parser, env: bool) -> bool {
    use cssparser::Token;
    match inner.next() {
        Ok(Token::Ident(name)) if env || name.starts_with("--") => {}
        _ => return false,
    }
    loop {
        match inner.next() {
            Err(_) | Ok(Token::Comma) => return true,
            Ok(Token::Number { int_value: Some(i), .. }) if env && *i >= 0 => {}
            Ok(_) => return false,
        }
    }
}

/// Replace every `var(--name[, fallback])` and `env(name[, fallback])` in `value`.
///
/// `lookup` answers with a custom property's computed value, or `None` if it is not set on this
/// element. Returns `None` when a reference cannot be resolved and has no fallback — the
/// declaration is then invalid at computed-value time, which is *not* the same as being dropped:
/// it still won the cascade, and what it computes to is `unset`.
pub fn substitute(value: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    substitute_at(value, lookup, 0)
}

/// The value of the environment variable `name` -- the user agent's, so Chrome 150's on a desktop (measured,
/// `env.html`): no notch, no virtual keyboard and the default text scale. A name it does not define -- the
/// `titlebar-area-*` of a window without its controls overlay, a `viewport-segment-*` off a foldable, a
/// known name given indices, a name in another case -- is not set, and its reference takes its fallback.
fn environment(name: &str) -> Option<&'static str> {
    match name {
        "safe-area-inset-top" | "safe-area-inset-right" | "safe-area-inset-bottom" | "safe-area-inset-left"
        | "safe-area-max-inset-top" | "safe-area-max-inset-right" | "safe-area-max-inset-bottom"
        | "safe-area-max-inset-left" | "keyboard-inset-top" | "keyboard-inset-right" | "keyboard-inset-bottom"
        | "keyboard-inset-left" | "keyboard-inset-width" | "keyboard-inset-height" => Some("0px"),
        "preferred-text-scale" => Some("1"),
        _ => None,
    }
}

fn substitute_at(value: &str, lookup: &dyn Fn(&str) -> Option<String>, depth: usize) -> Option<String> {
    if depth > MAX_DEPTH {
        return None;
    }
    let Some(at) = find_var(value) else {
        return Some(value.to_string());
    };
    let (open, close, env) = at;
    let inside = &value[open + 4..close];
    let (name, fallback) = split_reference(inside);

    if env {
        // An unused fallback is never substituted: Chrome 150 computes `env(safe-area-inset-left,
        // var(--missing))` 0px. A used one goes in as written, its whitespace kept -- `x env(nope,  3px) y`
        // is `x   3px y`, where a `var()`'s fallback is trimmed.
        let replacement = match environment(name) {
            Some(v) => v.to_string(),
            None => substitute_at(fallback?, lookup, depth + 1)?,
        };
        let rest = substitute_at(&value[close + 1..], lookup, depth)?;
        return Some(format!("{}{}{}", &value[..open], replacement, rest));
    }

    let replacement = match lookup(name) {
        // The referenced property's own value may reference others. It goes in whole: a value an `env()`
        // fallback left whitespace at the edge of keeps it (Chrome 150: `--v: env(nope, 3px)` is ` 3px`, and
        // `env(nope, var(--v))` `  3px`).
        Some(v) => substitute_at(&v, lookup, depth + 1)?,
        // An unset property falls back if the reference offered one, and invalidates if not. An
        // *empty* fallback is a valid fallback: `var(--x,)` is the empty token stream. The fallback is
        // trimmed: `var(--nope, 3px )` is `3px`.
        None => match fallback {
            Some(f) => substitute_at(f.trim(), lookup, depth + 1)?,
            None => return None,
        },
    };

    let rest = substitute_at(&value[close + 1..], lookup, depth)?;
    Some(format!("{}{}{}", &value[..open], replacement, rest))
}

/// Every custom property name `value` references, fallbacks included (a `var()` in a fallback
/// is a dependency too, css-variables-1 §2.3).
pub fn var_refs(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = value;
    while let Some((open, close, env)) = find_var(rest) {
        let (name, fallback) = split_reference(&rest[open + 4..close]);
        if !env {
            out.push(name.to_string());
        }
        if let Some(f) = fallback {
            out.extend(var_refs(f));
        }
        rest = &rest[close + 1..];
    }
    out
}

/// Which nodes of a dependency graph are in a CYCLE (a strongly connected component of two or more, or one
/// that references itself): those custom properties are invalid at computed-value time (Tarjan's algorithm).
pub fn cyclic_nodes(edges: &[Vec<usize>]) -> Vec<bool> {
    struct T<'a> {
        edges: &'a [Vec<usize>],
        index: Vec<Option<usize>>,
        low: Vec<usize>,
        on: Vec<bool>,
        stack: Vec<usize>,
        next: usize,
        cyclic: Vec<bool>,
    }
    fn visit(t: &mut T, v: usize) {
        t.index[v] = Some(t.next);
        t.low[v] = t.next;
        t.next += 1;
        t.stack.push(v);
        t.on[v] = true;
        for &w in &t.edges[v] {
            match t.index[w] {
                None => {
                    visit(t, w);
                    t.low[v] = t.low[v].min(t.low[w]);
                }
                Some(iw) if t.on[w] => t.low[v] = t.low[v].min(iw),
                _ => {}
            }
        }
        if Some(t.low[v]) == t.index[v] {
            let mut comp = Vec::new();
            while let Some(w) = t.stack.pop() {
                t.on[w] = false;
                comp.push(w);
                if w == v {
                    break;
                }
            }
            let cyc = comp.len() > 1 || t.edges[v].contains(&v);
            for w in comp {
                t.cyclic[w] = cyc;
            }
        }
    }
    let n = edges.len();
    let mut t = T { edges, index: vec![None; n], low: vec![0; n], on: vec![false; n], stack: Vec::new(), next: 0, cyclic: vec![false; n] };
    for v in 0..n {
        if t.index[v].is_none() {
            visit(&mut t, v);
        }
    }
    t.cyclic
}

/// The non-cyclic nodes, each after every node it depends on (a post-order), so resolving them in this order
/// finds every dependency already resolved.
pub fn dependency_order(edges: &[Vec<usize>], cyclic: &[bool]) -> Vec<usize> {
    fn walk(v: usize, edges: &[Vec<usize>], cyclic: &[bool], seen: &mut [bool], out: &mut Vec<usize>) {
        if seen[v] || cyclic[v] {
            return;
        }
        seen[v] = true;
        for &w in &edges[v] {
            walk(w, edges, cyclic, seen, out);
        }
        out.push(v);
    }
    let mut seen = vec![false; edges.len()];
    let mut out = Vec::with_capacity(edges.len());
    for v in 0..edges.len() {
        walk(v, edges, cyclic, &mut seen, &mut out);
    }
    out
}

/// The byte range of the first `var(` … `)` or `env(` … `)` in `value`, as (index of the function's name,
/// index of its `)`, whether it is `env()`).
///
/// Scanned rather than tokenized because the result has to be a *span of the original text*: the
/// parts around the reference are copied through untouched, whitespace and all.
fn find_var(value: &str) -> Option<(usize, usize, bool)> {
    let bytes = value.as_bytes();
    let mut i = 0usize;
    let mut in_string: Option<u8> = None;
    while i < bytes.len() {
        match in_string {
            Some(quote) => {
                if bytes[i] == b'\\' {
                    i += 1;
                } else if bytes[i] == quote {
                    in_string = None;
                }
            }
            None => {
                if bytes[i] == b'"' || bytes[i] == b'\'' {
                    in_string = Some(bytes[i]);
                } else if bytes.len() - i >= 4
                    // COMPARED AS BYTES, NEVER SLICED AS `&str`. `value[i..]` panics the moment `i`
                    // lands inside a multi-byte character, and this loop advances by one BYTE, so
                    // any non-ASCII character anywhere in a declaration aborted the process --
                    // `p{font-family: 물결, var(--f)}` took SIGABRT at this line. The match is
                    // identical: `var(` is four ASCII bytes, so a byte-wise ASCII-case-insensitive
                    // compare finds exactly the same occurrences, and a match cannot begin inside a
                    // character because a UTF-8 continuation byte is >= 0x80 and `b'v'` (or `b'e'`) is not.
                    // That is also what keeps `i` a char boundary for `close_paren` and for
                    // `substitute_at`'s `&value[..open]`.
                    && (bytes[i..i + 4].eq_ignore_ascii_case(b"var(") || bytes[i..i + 4].eq_ignore_ascii_case(b"env("))
                    && (i == 0 || !is_name_byte(bytes[i - 1]))
                {
                    let env = bytes[i].eq_ignore_ascii_case(&b'e');
                    return close_paren(value, i + 3).map(|close| (i, close, env));
                }
            }
        }
        i += 1;
    }
    None
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}

/// The index of the `)` matching the `(` at `open`.
fn close_paren(value: &str, open: usize) -> Option<usize> {
    let bytes = value.as_bytes();
    let mut depth = 0i32;
    let mut in_string: Option<u8> = None;
    for i in open..bytes.len() {
        match in_string {
            Some(quote) if bytes[i] == quote => in_string = None,
            Some(_) => {}
            None => match bytes[i] {
                b'"' | b'\'' => in_string = Some(bytes[i]),
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            },
        }
    }
    None
}

/// `--name, fallback` split at the *first* top-level comma. The fallback may itself contain commas
/// (`var(--font, Helvetica, sans-serif)` has one fallback, not two).
fn split_reference(inside: &str) -> (&str, Option<&str>) {
    let bytes = inside.as_bytes();
    let mut depth = 0i32;
    for i in 0..bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b',' if depth == 0 => return (inside[..i].trim(), Some(&inside[i + 1..])),
            _ => {}
        }
    }
    (inside.trim(), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn contains_var(value: &str) -> bool {
        references(value) == References::Some
    }

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn sub(value: &str, pairs: &[(&str, &str)]) -> Option<String> {
        let map = vars(pairs);
        substitute(value, &|name| map.get(name).cloned())
    }

    /// A declaration carrying a non-ASCII character used to ABORT THE PROCESS, not return wrong.
    ///
    /// `find_var` advances by one BYTE and used to ask `value[i..i + 4] == "var("`; slicing a `&str`
    /// at a byte index inside a multi-byte character panics, and the panic took SIGABRT with the
    /// whole engine. Found by L6b, whose live sweep recorded ten `errored` rows with every page
    /// field null -- what a crashed engine looks like -- one of them `fonts.adobe.com`.
    ///
    /// The corpus carries 50 Korean sites and an unquoted CJK font name is ordinary CSS, so these
    /// are not exotic inputs. Each case below aborted at `variables.rs:120` or `:121` before the fix.
    #[test]
    fn a_multibyte_character_is_not_a_slice_boundary() {
        // A reference AFTER a multi-byte character: the walk passes through 물 and 결 to reach it.
        assert_eq!(sub("물결 var(--f)", &[("--f", "x")]).as_deref(), Some("물결 x"));
        assert_eq!(sub("豈 var(--f)", &[("--f", "x")]).as_deref(), Some("豈 x"));
        assert_eq!(
            sub("물결, var(--f)", &[("--f", "sans-serif")]).as_deref(),
            Some("물결, sans-serif")
        );
        // A multi-byte character with NO reference must come back untouched rather than crash.
        assert_eq!(sub("물결", &[]).as_deref(), Some("물결"));
        // Inside a string, which took the other branch and so never crashed -- kept so the fix
        // cannot be "make the string branch swallow everything".
        assert_eq!(sub("'물결' var(--f)", &[("--f", "x")]).as_deref(), Some("'물결' x"));
        // A multi-byte character INSIDE the reference, in the fallback.
        assert_eq!(sub("var(--f, 물결)", &[]).as_deref(), Some("물결"));
        // And the ASCII behaviour the byte compare must not change: case-insensitive, and a
        // reference that is part of a longer name is not a reference.
        assert_eq!(sub("VAR(--c)", &[("--c", "red")]).as_deref(), Some("red"));
        assert_eq!(sub("물결 novar(--c)", &[("--c", "red")]).as_deref(), Some("물결 novar(--c)"));
    }

    #[test]
    fn a_reference_becomes_the_value_it_names() {
        assert_eq!(sub("var(--c)", &[("--c", "red")]).as_deref(), Some("red"));
        assert_eq!(
            sub("1px solid var(--c)", &[("--c", "red")]).as_deref(),
            Some("1px solid red")
        );
    }

    #[test]
    fn several_references_in_one_value_all_resolve() {
        assert_eq!(
            sub("var(--a) var(--b) var(--a)", &[("--a", "1px"), ("--b", "2px")]).as_deref(),
            Some("1px 2px 1px")
        );
    }

    #[test]
    fn a_reference_can_name_a_property_that_is_itself_a_reference() {
        assert_eq!(
            sub("var(--a)", &[("--a", "var(--b)"), ("--b", "blue")]).as_deref(),
            Some("blue")
        );
    }

    // An unresolvable reference with no fallback makes the declaration invalid at computed-value
    // time -- which is not the same as dropping it. The caller turns this into `unset`.
    #[test]
    fn an_unset_property_with_no_fallback_invalidates_the_declaration() {
        assert_eq!(sub("var(--missing)", &[]), None);
        assert_eq!(sub("1px solid var(--missing)", &[]), None);
    }

    #[test]
    fn a_fallback_is_used_only_when_the_property_is_unset() {
        assert_eq!(sub("var(--missing, red)", &[]).as_deref(), Some("red"));
        assert_eq!(sub("var(--c, red)", &[("--c", "blue")]).as_deref(), Some("blue"));
        // An empty fallback is a fallback: `var(--x,)` is the empty token stream, not a failure.
        assert_eq!(sub("var(--missing,)", &[]).as_deref(), Some(""));
    }

    // The fallback is everything after the *first* comma, so this is one fallback and not two.
    #[test]
    fn a_fallback_may_contain_commas_of_its_own() {
        assert_eq!(
            sub("var(--font, Helvetica, sans-serif)", &[]).as_deref(),
            Some("Helvetica, sans-serif")
        );
    }

    #[test]
    fn a_fallback_may_itself_be_a_reference() {
        assert_eq!(
            sub("var(--missing, var(--other))", &[("--other", "teal")]).as_deref(),
            Some("teal")
        );
        assert_eq!(sub("var(--missing, var(--also-missing))", &[]), None);
    }

    // A cycle makes every declaration in it invalid, which is what running out of depth produces.
    #[test]
    fn a_cycle_invalidates_rather_than_hanging() {
        assert_eq!(sub("var(--a)", &[("--a", "var(--b)"), ("--b", "var(--a)")]), None);
        assert_eq!(sub("var(--a)", &[("--a", "var(--a)")]), None);
    }

    // `content: "var(--x)"` is a string containing those characters, not a reference. Treating it
    // as one would defer a declaration that resolves perfectly well now.
    #[test]
    fn a_reference_inside_a_string_is_not_a_reference() {
        assert!(!contains_var(r#""var(--x)""#));
        assert_eq!(sub(r#""var(--x)""#, &[]).as_deref(), Some(r#""var(--x)""#));
        assert!(!contains_var("varsity"), "nor is a word that starts with the same letters");
        assert!(!contains_var("--myvar(1)"));
    }

    #[test]
    fn detection_looks_inside_nested_functions() {
        assert!(contains_var("calc(1px + var(--x))"));
        assert!(contains_var("rgb(var(--r) 0 0)"));
        assert!(contains_var("VAR(--x)"), "case-insensitive, like every CSS function");
        assert!(!contains_var("calc(1px + 2px)"));
        assert!(!contains_var("red"));
    }

    #[test]
    fn a_value_with_no_references_comes_back_unchanged() {
        assert_eq!(sub("1px solid red", &[]).as_deref(), Some("1px solid red"));
        assert_eq!(sub("", &[]).as_deref(), Some(""));
    }

    // Chrome 150 on a desktop, measured (`env.html`, `env2.html`, `env3.html`).
    #[test]
    fn an_environment_variable_is_the_desktops() {
        assert!(contains_var("max(22px, env(safe-area-inset-left))"));
        assert!(contains_var("ENV(safe-area-inset-left)"));
        assert_eq!(references("env()"), References::Invalid);
        assert_eq!(references("env(, 6px)"), References::Invalid);
        assert_eq!(references("1px env(1)"), References::Invalid);
        assert_eq!(references("env(nope 1 2, 3px)"), References::Some);
        assert_eq!(references("env(nope 1.5, 3px)"), References::Invalid);
        assert_eq!(references("env(nope -1, 3px)"), References::Invalid);
        assert_eq!(references("env(nope a, 3px)"), References::Invalid);
        assert_eq!(references("var(x)"), References::Invalid);
        assert_eq!(references("var()"), References::Invalid);
        assert_eq!(references("var(--x 1)"), References::Invalid);
        assert_eq!(references("var(--x,1px)"), References::Some);
        assert_eq!(references("env(nope, var(x))"), References::Invalid);
        assert_eq!(sub("max(22px, env(safe-area-inset-left))", &[]).as_deref(), Some("max(22px, 0px)"));
        assert_eq!(sub("env(safe-area-inset-top) env(keyboard-inset-width)", &[]).as_deref(), Some("0px 0px"));
        assert_eq!(sub("env( safe-area-inset-left ,4px)", &[]).as_deref(), Some("0px"));
        assert_eq!(sub("env(preferred-text-scale)", &[]).as_deref(), Some("1"));
        // a name it does not define takes the fallback as written, whitespace and all, or invalidates
        assert_eq!(sub("x env(nope,  3px) y", &[]).as_deref(), Some("x   3px y"));
        assert_eq!(sub("env(titlebar-area-x, 9px)", &[]).as_deref(), Some(" 9px"));
        assert_eq!(sub("env(SAFE-AREA-INSET-LEFT, 4px)", &[]).as_deref(), Some(" 4px"));
        assert_eq!(sub("env(safe-area-inset-left 1, 4px)", &[]).as_deref(), Some(" 4px"));
        assert_eq!(sub("env(nope,)", &[]).as_deref(), Some(""));
        assert_eq!(sub("env(nope)", &[]), None);
        // a fallback's own references resolve when it is used, and are never looked at when it is not
        assert_eq!(sub("env(nope, var(--m, 8px))", &[]).as_deref(), Some(" 8px"));
        assert_eq!(sub("env(safe-area-inset-left, var(--missing))", &[]).as_deref(), Some("0px"));
        assert_eq!(sub("env(nope, var(--v))", &[("--v", " 3px")]).as_deref(), Some("  3px"));
        assert!(var_refs("env(nope, var(--m))") == ["--m"]);
    }
}
