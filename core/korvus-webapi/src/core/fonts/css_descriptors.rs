//! `core/fonts/css_descriptors.rs` -- the CSS grammar behind `FontFace`'s descriptors and
//! `FontFaceSet`'s `font` argument.
//!
//! WHY A GRAMMAR AND NOT A LIST OF ALLOWED STRINGS. Every `FontFace` descriptor is a CSS value, and
//! the recorded answers are the CSS defaults with the CSS grammar's own rejection: `o.style = "x"` is
//! a `SyntaxError` because `"x"` is not a `<font-style>`, not because a list copied from a recorded
//! case does not contain it. A list would pass every case in the bundle and be wrong for every value
//! not in it -- the exact failure `MediaSource.isTypeSupported`'s constant `false` was.
//!
//! THIS IS A PORT, NOT A RE-DERIVATION. ~280 lines of `dom/09k-media-rtc.js` are the recorded
//! behaviour the capture grades against, so each function below is the JS function of the same name,
//! transcribed -- including where it is LENIENT (`parseVariant` accepts any identifier run;
//! `parseSource` accepts any `format(...)` tail). The JS regexes are hand-written scanners here rather
//! than a regex dependency, and each one names the pattern it replaces so the two can be compared.
//!
//! WHAT "JS SEMANTICS" MEANS IN THE DETAILS, since the tests turn on them:
//!   * `\s` is JavaScript's whitespace set, not `char::is_whitespace` -- they differ (JS includes
//!     U+FEFF and excludes U+0085).
//!   * `\\.` in a JS regex is a backslash plus any character EXCEPT a line terminator.
//!   * the ident pattern's non-ASCII class (0x0080 through 0xFFFF) is over UTF-16 code units, so an
//!     astral character is two surrogates and both are inside it -- which is `c >= '\u{0080}'` over
//!     `char`s.
//!   * `String#length` is the UTF-16 length (`parseFeatureSettings` tests a tag for `length === 6`).
//!   * `String(n)` for a `Number` is the shortest round-tripping form, which Rust's `{}` also gives
//!     for every magnitude a font descriptor carries; [`js_num`] handles the two places they differ
//!     that can occur here (`-0`, and integers below 1e21).

/// JavaScript's `\s` (and the set `String#trim` removes). Deliberately not `char::is_whitespace`.
fn is_js_ws(c: char) -> bool {
    matches!(c,
        '\u{9}' | '\u{a}' | '\u{b}' | '\u{c}' | '\u{d}' | '\u{20}' | '\u{a0}' | '\u{1680}'
        | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}

/// What `.` does NOT match in a JS regex without the `s` flag.
fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// JS `String#trim`.
pub fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_ws)
}

/// The JS `ws()` helper: `String(s).trim().replace(/\s+/g, " ")` -- trim, then collapse every run of
/// whitespace to one space. Every descriptor parser starts here, which is why `split(' ')` downstream
/// is safe.
pub fn ws(s: &str) -> String {
    let t = js_trim(s);
    let mut out = String::with_capacity(t.len());
    let mut in_run = false;
    for c in t.chars() {
        if is_js_ws(c) {
            if !in_run {
                out.push(' ');
                in_run = true;
            }
        } else {
            out.push(c);
            in_run = false;
        }
    }
    out
}

// ---- the numeric scanners (the JS `NUM` sub-pattern) ---------------------------------------------

/// Scan the JS `NUM` pattern -- `[-+]?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?` -- at the start of `s`,
/// returning what follows it. `None` is "no number here".
fn scan_number(s: &str) -> Option<&str> {
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    if i < b.len() && b[i].is_ascii_digit() {
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i < b.len() && b[i] == b'.' {
            i += 1;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        }
    } else if i < b.len() && b[i] == b'.' {
        i += 1;
        let d0 = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == d0 {
            return None;
        }
    } else {
        return None;
    }
    // The exponent is optional, so a half-written one is simply not consumed (`"1e"` leaves `"e"`,
    // which then fails the caller's end-of-input check -- exactly what the regex's `$` does).
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        let d0 = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j > d0 {
            i = j;
        }
    }
    Some(&s[i..])
}

/// JS `rxNum`: `^NUM$`.
pub fn is_number(s: &str) -> bool {
    scan_number(s).map(|r| r.is_empty()).unwrap_or(false)
}

/// JS `rxPct`: `^NUM%$`.
pub fn is_percentage(s: &str) -> bool {
    scan_number(s).map(|r| r == "%").unwrap_or(false)
}

/// JS `rxAngle`: `^NUM(?:deg|grad|rad|turn)$`. Callers lowercase first, as the JS does.
fn is_angle(s: &str) -> bool {
    scan_number(s).map(|r| matches!(r, "deg" | "grad" | "rad" | "turn")).unwrap_or(false)
}

/// JS `Number(s)` for a string that already matched `rxNum` (so it cannot be `NaN`).
pub fn js_number(s: &str) -> f64 {
    s.parse::<f64>().unwrap_or(f64::NAN)
}

/// JS `parseFloat(s)` for a string that already matched `rxPct` -- the numeric prefix, `%` dropped.
fn js_parse_float_pct(s: &str) -> f64 {
    js_number(&s[..s.len() - 1])
}

/// JS `String(n)` for the finite numbers a font descriptor can carry. Rust's `{}` is the shortest
/// round-tripping form, the same rule JS uses, so the two agree everywhere except `-0` (JS prints
/// `"0"`) and magnitudes at or past 1e21 (JS switches to exponent form; Rust does not). Neither can
/// reach a descriptor through a grammar that caps weights at 1000 and percentages at a parsed float,
/// but `-0` is one keystroke away (`"-0"` is a legal `<number>`), so it is handled.
pub fn js_num(n: f64) -> String {
    if n == 0.0 {
        return "0".to_string();
    }
    format!("{n}")
}

// ---- the token scanners --------------------------------------------------------------------------

/// JS `rxString`: `^(?:"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*')$` -- a whole CSS string, quotes included.
pub fn is_css_string(s: &str) -> bool {
    let mut it = s.chars();
    let Some(q) = it.next() else { return false };
    if q != '"' && q != '\'' {
        return false;
    }
    let rest: Vec<char> = it.collect();
    let mut i = 0;
    while i < rest.len() {
        let c = rest[i];
        if c == q {
            // The closing quote: the regex's `$` says nothing may follow it.
            return i == rest.len() - 1;
        }
        if c == '\\' {
            // `\\.` -- one more character, and `.` does not match a line terminator.
            if i + 1 >= rest.len() || is_line_terminator(rest[i + 1]) {
                return false;
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    false
}

/// JS `rxIdent`: a CSS `<custom-ident>` --
/// `^-?(?:[A-Za-z_\u0080-\uffff]|\\.)(?:[A-Za-z0-9_\u0080-\uffff-]|\\.)*$` (the class spelled in hex).
pub fn is_css_ident(s: &str) -> bool {
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    // `-?` is greedy but a bare `-` cannot start the required first atom either way, so the two
    // backtracking outcomes agree: `"-"` and `"--a"` are both rejected.
    if i < c.len() && c[i] == '-' {
        i += 1;
    }
    if i >= c.len() {
        return false;
    }
    if c[i] == '\\' {
        if i + 1 >= c.len() || is_line_terminator(c[i + 1]) {
            return false;
        }
        i += 2;
    } else if c[i].is_ascii_alphabetic() || c[i] == '_' || c[i] >= '\u{0080}' {
        i += 1;
    } else {
        return false;
    }
    while i < c.len() {
        if c[i] == '\\' {
            if i + 1 >= c.len() || is_line_terminator(c[i + 1]) {
                return false;
            }
            i += 2;
        } else if c[i].is_ascii_alphanumeric() || c[i] == '_' || c[i] == '-' || c[i] >= '\u{0080}' {
            i += 1;
        } else {
            return false;
        }
    }
    true
}

/// The CSS-wide keywords. They are legal CSS values and are NOT family names or variants.
const CSSWIDE: &[&str] = &["initial", "inherit", "unset", "revert", "revert-layer", "default"];

fn is_css_wide(lower: &str) -> bool {
    CSSWIDE.contains(&lower)
}

// ---- the descriptors -----------------------------------------------------------------------------

/// The generic family keywords, which a family NAME given to the `FontFace` constructor must not be
/// mistaken for -- so they serialize quoted.
const GENERIC_FAMILIES: &[&str] = &[
    "serif", "sans-serif", "cursive", "fantasy", "monospace", "system-ui", "math", "emoji", "fangsong",
    "ui-serif", "ui-sans-serif", "ui-monospace", "ui-rounded", "-webkit-body", "-webkit-pictograph",
];

/// Whether `s` is ONE identifier token as the CSS tokenizer spells it with no escape: a name-start
/// code point (letter, `_`, non-ASCII) optionally after a single `-`, then name code points.
fn is_plain_ident(s: &str) -> bool {
    let c: Vec<char> = s.chars().collect();
    let start = |ch: char| ch.is_ascii_alphabetic() || ch == '_' || ch >= '\u{0080}';
    let i = usize::from(c.first() == Some(&'-'));
    if i >= c.len() || !start(c[i]) {
        return false;
    }
    c[i..].iter().all(|&ch| start(ch) || ch.is_ascii_digit() || ch == '-')
}

/// CSSOM "serialize a string": double quotes, `"` and `\` escaped, a control character as a hex
/// escape followed by a space, NUL as U+FFFD.
pub fn serialize_css_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\0' => out.push('\u{FFFD}'),
            '\u{1}'..='\u{1f}' | '\u{7f}' => out.push_str(&format!("\\{:x} ", ch as u32)),
            '"' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// The family the `FontFace` CONSTRUCTOR stores: the string itself when it is a single plain
/// identifier that is neither a CSS-wide keyword, `default`, nor a generic family, and otherwise the
/// string serialized as a CSS string. It never fails -- Chrome 150, measured:
///
/// ```text
/// "probe" -> probe      "a-b" -> a-b       "-x" -> -x          "é" -> é
/// "Open Sans" -> "\"Open Sans\""        "1bad" -> "\"1bad\""   "--x" -> "\"--x\""
/// "inherit" / "default" / "serif" -> quoted                    "x\\y" -> "\"x\\\\y\""
/// "" -> "\"\""        "a\nb" -> "\"a\\a b\""
/// ```
pub fn serialize_family(v: &str) -> String {
    let lower = v.to_ascii_lowercase();
    if is_plain_ident(v) && !is_css_wide(&lower) && !GENERIC_FAMILIES.contains(&lower.as_str()) {
        v.to_string()
    } else {
        serialize_css_string(v)
    }
}

/// `<family-name>`: a string, or one or more identifiers. A CSS-wide keyword is not a family name.
pub fn parse_family(v: &str) -> Option<String> {
    let t = ws(v);
    if t.is_empty() {
        return None;
    }
    if is_css_string(&t) {
        return Some(t);
    }
    for p in t.split(' ') {
        if !is_css_ident(p) || is_css_wide(&p.to_lowercase()) {
            return None;
        }
    }
    Some(t)
}

/// `normal | italic | oblique | oblique <angle>{1,2}`.
fn parse_style(v: &str) -> Option<String> {
    let t = ws(v).to_lowercase();
    if t == "normal" || t == "italic" || t == "oblique" {
        return Some(t);
    }
    let parts: Vec<&str> = t.split(' ').collect();
    if parts[0] != "oblique" || parts.len() > 3 {
        return None;
    }
    for p in &parts[1..] {
        if !is_angle(p) {
            return None;
        }
    }
    if parts.len() > 1 {
        Some(t)
    } else {
        None
    }
}

fn one_weight(p: &str) -> bool {
    if p == "normal" || p == "bold" {
        return true;
    }
    is_number(p) && {
        let n = js_number(p);
        (1.0..=1000.0).contains(&n)
    }
}

/// `normal | bold | <number [1,1000]>`, one or two of them (a range).
fn parse_weight(v: &str) -> Option<String> {
    let t = ws(v).to_lowercase();
    let parts: Vec<&str> = t.split(' ').collect();
    if parts.len() > 2 {
        return None;
    }
    for p in &parts {
        if !one_weight(p) {
            return None;
        }
    }
    Some(t)
}

const STRETCH: &[&str] = &[
    "normal",
    "ultra-condensed",
    "extra-condensed",
    "condensed",
    "semi-condensed",
    "semi-expanded",
    "expanded",
    "extra-expanded",
    "ultra-expanded",
];

fn one_stretch(p: &str) -> bool {
    STRETCH.contains(&p) || (is_percentage(p) && js_parse_float_pct(p) >= 0.0)
}

fn parse_stretch(v: &str) -> Option<String> {
    let t = ws(v).to_lowercase();
    let parts: Vec<&str> = t.split(' ').collect();
    if parts.len() > 2 {
        return None;
    }
    for p in &parts {
        if !one_stretch(p) {
            return None;
        }
    }
    Some(t)
}

/// `U+<hex>`, `U+<hex>-<hex>`, `U+<hex with ?>`, comma-separated. Serialised back UPPERCASE with no
/// leading zeros, which is why the default reads `U+0-10FFFF` and not `U+000000-10FFFF`.
fn parse_unicode_range(v: &str) -> Option<String> {
    let mut out: Vec<String> = Vec::new();
    // NOTE: the JS splits `String(v)`, not `ws(v)` -- each entry is trimmed on its own instead.
    for raw in v.split(',') {
        let t = js_trim(raw);
        let (lo, hi) = match match_range(t).or_else(|| match_wildcard(t)) {
            Some(p) => p,
            None => return None,
        };
        if !(lo <= hi) || hi > 0x10ffff {
            return None;
        }
        out.push(if lo == hi {
            format!("U+{:X}", lo)
        } else {
            format!("U+{:X}-{:X}", lo, hi)
        });
    }
    if out.is_empty() {
        None
    } else {
        Some(out.join(", "))
    }
}

/// `^[uU]\+([0-9a-fA-F]{1,6})(?:-([0-9a-fA-F]{1,6}))?$`
fn match_range(t: &str) -> Option<(u32, u32)> {
    let rest = t.strip_prefix("u+").or_else(|| t.strip_prefix("U+"))?;
    let (a, b) = match rest.split_once('-') {
        Some((a, b)) => (a, Some(b)),
        None => (rest, None),
    };
    let hex = |s: &str| -> Option<u32> {
        if s.is_empty() || s.len() > 6 || !s.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        u32::from_str_radix(s, 16).ok()
    };
    let lo = hex(a)?;
    let hi = match b {
        Some(b) => hex(b)?,
        None => lo,
    };
    Some((lo, hi))
}

/// `^[uU]\+([0-9a-fA-F]{0,5})(\?{1,6})$`
fn match_wildcard(t: &str) -> Option<(u32, u32)> {
    let rest = t.strip_prefix("u+").or_else(|| t.strip_prefix("U+"))?;
    let q_at = rest.find('?')?;
    let (digits, qs) = rest.split_at(q_at);
    if digits.len() > 5 || !digits.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    if qs.is_empty() || qs.len() > 6 || !qs.bytes().all(|c| c == b'?') {
        return None;
    }
    // The JS checks the COMBINED length after the match, so `U+12345??` (5 + 2) is rejected here and
    // not by the pattern.
    if digits.len() + qs.len() > 6 {
        return None;
    }
    let lo = u32::from_str_radix(&format!("{digits}{}", "0".repeat(qs.len())), 16).ok()?;
    let hi = u32::from_str_radix(&format!("{digits}{}", "F".repeat(qs.len())), 16).ok()?;
    Some((lo, hi))
}

/// `normal | <feature-tag-value>#`, where a tag is a 4-character STRING plus an optional integer or
/// `on`/`off`. Serialised with `on` dropped, `off` written `0` and `1` dropped.
fn parse_feature_settings(v: &str) -> Option<String> {
    let t = ws(v);
    if t.to_lowercase() == "normal" {
        return Some("normal".to_string());
    }
    let mut out: Vec<String> = Vec::new();
    for raw in t.split(',') {
        let p: Vec<&str> = js_trim(raw).split(' ').collect();
        if p.len() > 2 || !is_css_string(p[0]) || p[0].encode_utf16().count() != 6 {
            return None;
        }
        if p.len() == 2 {
            let a = p[1].to_lowercase();
            if a == "on" {
                out.push(p[0].to_string());
                continue;
            }
            if a == "off" {
                out.push(format!("{} 0", p[0]));
                continue;
            }
            if p[1].is_empty() || !p[1].bytes().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let n = js_number(p[1]);
            out.push(if n == 1.0 { p[0].to_string() } else { format!("{} {}", p[0], js_num(n)) });
            continue;
        }
        out.push(p[0].to_string());
    }
    if out.is_empty() {
        None
    } else {
        Some(out.join(", "))
    }
}

/// `normal | [<string> <number>]#`
fn parse_variation_settings(v: &str) -> Option<String> {
    let t = ws(v);
    if t.to_lowercase() == "normal" {
        return Some("normal".to_string());
    }
    let mut out: Vec<String> = Vec::new();
    for raw in t.split(',') {
        let p: Vec<&str> = js_trim(raw).split(' ').collect();
        if p.len() != 2 || !is_css_string(p[0]) || p[0].encode_utf16().count() != 6 || !is_number(p[1]) {
            return None;
        }
        out.push(format!("{} {}", p[0], js_num(js_number(p[1]))));
    }
    if out.is_empty() {
        None
    } else {
        Some(out.join(", "))
    }
}

const DISPLAY: &[&str] = &["auto", "block", "swap", "fallback", "optional"];

fn parse_display(v: &str) -> Option<String> {
    let t = ws(v).to_lowercase();
    if DISPLAY.contains(&t.as_str()) {
        Some(t)
    } else {
        None
    }
}

/// `normal | <percentage [0,infinity]>` -- the three metric overrides.
fn parse_override(v: &str) -> Option<String> {
    let t = ws(v).to_lowercase();
    if t == "normal" {
        return Some("normal".to_string());
    }
    if is_percentage(&t) && js_parse_float_pct(&t) >= 0.0 {
        Some(format!("{}%", js_num(js_parse_float_pct(&t))))
    } else {
        None
    }
}

/// `<percentage [0,infinity]>` -- `size-adjust`, which has NO `normal` (its initial value is `100%`).
fn parse_size_adjust(v: &str) -> Option<String> {
    let t = ws(v).to_lowercase();
    if is_percentage(&t) && js_parse_float_pct(&t) >= 0.0 {
        Some(format!("{}%", js_num(js_parse_float_pct(&t))))
    } else {
        None
    }
}

/// `font-variant` is a shorthand of many longhands; only `normal`, `none` and the feature keywords are
/// legal here. LENIENT on purpose (any identifier run that is not a CSS-wide keyword), because that is
/// what the recorded behaviour is.
fn parse_variant(v: &str) -> Option<String> {
    let t = ws(v).to_lowercase();
    if t == "normal" || t == "none" {
        return Some(t);
    }
    for p in t.split(' ') {
        if !is_css_ident(p) || is_css_wide(p) {
            return None;
        }
    }
    Some(t)
}

// ---- the `src` descriptor ------------------------------------------------------------------------

/// Split a CSS comma-separated list at its TOP-LEVEL commas only. `url(data:,)` is ONE entry, not two:
/// a naive split cuts it in half and then rejects both halves, which is how the most ordinary font
/// source there is came out a `SyntaxError`.
pub fn split_top(t: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let c: Vec<char> = t.chars().collect();
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut start = 0usize;
    let mut i = 0usize;
    while i < c.len() {
        let ch = c[i];
        match quote {
            Some(q) => {
                if ch == '\\' {
                    i += 1;
                } else if ch == q {
                    quote = None;
                }
            }
            None => {
                if ch == '"' || ch == '\'' {
                    quote = Some(ch);
                } else if ch == '(' {
                    depth += 1;
                } else if ch == ')' {
                    if depth > 0 {
                        depth -= 1;
                    }
                } else if ch == ',' && depth == 0 {
                    out.push(c[start..i].iter().collect());
                    start = i + 1;
                }
            }
        }
        i += 1;
    }
    out.push(c[start.min(c.len())..].iter().collect());
    out
}

/// `url\(\s*(?:<string>|[^)\s'"]*)\s*\)(?:\s+(?:format|tech)\(.*\))?` -- case-insensitive, anchored.
fn is_url_source(p: &str) -> bool {
    let c: Vec<char> = p.chars().collect();
    if c.len() < 4 {
        return false;
    }
    let head: String = c[..4].iter().collect();
    if !head.eq_ignore_ascii_case("url(") {
        return false;
    }
    let mut i = 4;
    while i < c.len() && is_js_ws(c[i]) {
        i += 1;
    }
    // The alternation is ordered: the quoted forms are tried first, then the bare one. The bare form
    // cannot begin with a quote, so when a quoted parse fails the fallback can only match empty.
    let mut candidates: Vec<usize> = Vec::new();
    if let Some(end) = scan_quoted(&c, i) {
        candidates.push(end);
    }
    let mut j = i;
    while j < c.len() && c[j] != ')' && !is_js_ws(c[j]) && c[j] != '\'' && c[j] != '"' {
        j += 1;
    }
    candidates.push(j);
    for end in candidates {
        let mut k = end;
        while k < c.len() && is_js_ws(c[k]) {
            k += 1;
        }
        if k >= c.len() || c[k] != ')' {
            continue;
        }
        k += 1;
        if k == c.len() {
            return true;
        }
        if url_tail_matches(&c[k..]) {
            return true;
        }
    }
    false
}

/// `\s+(?:format|tech)\(.*\)$` -- the optional tail after `url(...)`. LENIENT by design: the JS
/// accepts ANY text between the parentheses, so a `format("woff2")` and a `format(nonsense)` are both
/// a source. `.*` is greedy and matches no line terminator, so the last character must be `)` and no
/// newline may appear before it.
fn url_tail_matches(t: &[char]) -> bool {
    let mut i = 0;
    while i < t.len() && is_js_ws(t[i]) {
        i += 1;
    }
    if i == 0 {
        return false; // `\s+` needs at least one
    }
    for kw in ["format", "tech"] {
        let n = kw.len();
        if i + n + 1 <= t.len() {
            let word: String = t[i..i + n].iter().collect();
            if word.eq_ignore_ascii_case(kw) && t[i + n] == '(' {
                let body = &t[i + n + 1..];
                if body.last() == Some(&')') && !body[..body.len() - 1].iter().any(|&ch| is_line_terminator(ch)) {
                    return true;
                }
            }
        }
    }
    false
}

/// `local\(\s*(?:<string>|[^)]+)\s*\)` -- case-insensitive, anchored. The bare alternative allows
/// whitespace and quotes, so `local("x" y)` is a source: the quoted parse fails at the `\)` and the
/// regex backtracks into `[^)]+`. Both branches are tried here for the same reason.
fn is_local_source(p: &str) -> bool {
    let c: Vec<char> = p.chars().collect();
    if c.len() < 6 {
        return false;
    }
    let head: String = c[..6].iter().collect();
    if !head.eq_ignore_ascii_case("local(") {
        return false;
    }
    let mut i = 6;
    while i < c.len() && is_js_ws(c[i]) {
        i += 1;
    }
    let mut candidates: Vec<usize> = Vec::new();
    if let Some(end) = scan_quoted(&c, i) {
        candidates.push(end);
    }
    // The bare form is `<custom-ident>+`: identifiers separated by whitespace, nothing else. It was
    // "anything up to `)`", which took `local("x" y)` -- a string followed by an identifier -- where
    // Chrome 150 answers `status` "error" (measured).
    let mut j = i;
    while j < c.len() && c[j] != ')' {
        j += 1;
    }
    if j > i {
        let body: String = c[i..j].iter().collect();
        if body.split(|ch: char| is_js_ws(ch)).filter(|w| !w.is_empty()).all(is_css_ident) {
            candidates.push(j);
        }
    }
    for end in candidates {
        let mut k = end;
        while k < c.len() && is_js_ws(c[k]) {
            k += 1;
        }
        if k + 1 == c.len() && c[k] == ')' {
            return true;
        }
    }
    false
}

/// `"(?:[^"\\]|\\.)*"` (or the `'` form) starting at `i`; returns the index just past the closing
/// quote. `None` when `c[i]` is not a quote or the string never closes.
fn scan_quoted(c: &[char], i: usize) -> Option<usize> {
    if i >= c.len() {
        return None;
    }
    let q = c[i];
    if q != '"' && q != '\'' {
        return None;
    }
    let mut k = i + 1;
    while k < c.len() {
        if c[k] == q {
            return Some(k + 1);
        }
        if c[k] == '\\' {
            if k + 1 >= c.len() || is_line_terminator(c[k + 1]) {
                return None;
            }
            k += 2;
        } else {
            k += 1;
        }
    }
    None
}

/// The `src` descriptor: `[<url> [format(<string>)]? | local(<family-name>)]#`.
pub fn parse_source(v: &str) -> Option<String> {
    let t = ws(v);
    if t.is_empty() {
        return None;
    }
    if source_list_ok(&t) {
        return Some(t);
    }
    // The CSS tokenizer CLOSES a block the input left open at EOF, so `url(a` is `url(a)` -- Chrome
    // 150, measured: `new FontFace("probe", "url(a").status` is "unloaded" where "x" and "local()"
    // are "error". One missing `)` at the very end is the only repair it makes here.
    if open_parens(&t) == 1 {
        let closed = format!("{t})");
        if source_list_ok(&closed) {
            return Some(closed);
        }
    }
    None
}

fn source_list_ok(t: &str) -> bool {
    split_top(t).iter().all(|raw| {
        let p = js_trim(raw);
        is_url_source(p) || is_local_source(p)
    })
}

/// How many `(` outside quotes the text leaves unclosed.
fn open_parens(t: &str) -> usize {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut chars = t.chars();
    while let Some(ch) = chars.next() {
        match quote {
            Some(q) => {
                if ch == '\\' {
                    chars.next();
                } else if ch == q {
                    quote = None;
                }
            }
            None => match ch {
                '"' | '\'' => quote = Some(ch),
                '(' => depth += 1,
                ')' => depth = depth.saturating_sub(1),
                _ => {}
            },
        }
    }
    if quote.is_some() { 0 } else { depth }
}

// ---- the descriptor table ------------------------------------------------------------------------

/// One `FontFace` descriptor: the IDL attribute name, its initial value, and its grammar. The ORDER is
/// the JS `FF_DESCRIPTORS` order, which is the order the constructor applies an init dictionary in --
/// not the capture's member order, which `install_font_face` follows instead.
pub struct Descriptor {
    pub name: &'static str,
    pub initial: &'static str,
    pub parse: fn(&str) -> Option<String>,
}

pub const DESCRIPTORS: &[Descriptor] = &[
    Descriptor { name: "style", initial: "normal", parse: parse_style },
    Descriptor { name: "weight", initial: "normal", parse: parse_weight },
    Descriptor { name: "stretch", initial: "normal", parse: parse_stretch },
    Descriptor { name: "unicodeRange", initial: "U+0-10FFFF", parse: parse_unicode_range },
    Descriptor { name: "variant", initial: "normal", parse: parse_variant },
    Descriptor { name: "featureSettings", initial: "normal", parse: parse_feature_settings },
    Descriptor { name: "variationSettings", initial: "normal", parse: parse_variation_settings },
    Descriptor { name: "display", initial: "auto", parse: parse_display },
    Descriptor { name: "ascentOverride", initial: "normal", parse: parse_override },
    Descriptor { name: "descentOverride", initial: "normal", parse: parse_override },
    Descriptor { name: "lineGapOverride", initial: "normal", parse: parse_override },
    Descriptor { name: "sizeAdjust", initial: "100%", parse: parse_size_adjust },
];

pub fn descriptor(name: &str) -> Option<&'static Descriptor> {
    DESCRIPTORS.iter().find(|d| d.name == name)
}

// ---- the `font` shorthand, enough of it to answer `check()` --------------------------------------

const SYSTEM_FONTS: &[&str] = &["caption", "icon", "menu", "message-box", "small-caption", "status-bar"];
const SIZE_KEYWORDS: &[&str] = &[
    "xx-small", "x-small", "small", "medium", "large", "x-large", "xx-large", "xxx-large", "larger", "smaller",
];
const LENGTH_UNITS: &[&str] =
    &["px", "em", "rem", "ex", "ch", "vw", "vh", "vmin", "vmax", "cm", "mm", "in", "pt", "pc", "q", "%"];

/// JS `rxLen` with the `i` flag: `^NUM(?:px|em|...|q|%)$`, case-insensitive.
fn is_length(s: &str) -> bool {
    scan_number(s)
        .map(|r| {
            let lower = r.to_lowercase();
            LENGTH_UNITS.contains(&lower.as_str())
        })
        .unwrap_or(false)
}

fn is_size(p: &str) -> bool {
    let head = p.split('/').next().unwrap_or(p);
    SIZE_KEYWORDS.contains(&head.to_lowercase().as_str()) || is_length(head) || head == "0"
}

/// The `font` shorthand: an optional style/variant/weight/stretch run, then a REQUIRED `<font-size>`
/// (with an optional `/<line-height>`), then a REQUIRED family list. `"x"` alone has no size and no
/// family, which is why `document.fonts.check("x")` is a `SyntaxError` and not `false`.
pub fn parse_font_shorthand(v: &str) -> Option<Vec<String>> {
    let t = ws(v);
    if t.is_empty() {
        return None;
    }
    let lower = t.to_lowercase();
    if SYSTEM_FONTS.contains(&lower.as_str()) {
        return Some(vec![lower]);
    }
    let parts: Vec<&str> = t.split(' ').collect();
    let mut i = 0usize;
    // The prefix run: at most four values, none of which may be a size.
    while i < parts.len() && i < 4 && !is_size(parts[i]) {
        i += 1;
    }
    if i >= parts.len() || !is_size(parts[i]) {
        return None;
    }
    i += 1;
    let rest = parts[i..].join(" ");
    if rest.is_empty() {
        return None;
    }
    let mut families = Vec::new();
    for raw in split_top(&rest) {
        families.push(parse_family(&raw)?);
    }
    if families.is_empty() {
        None
    } else {
        Some(families)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_number_scanner_is_the_js_num_pattern() {
        for good in ["1", "+1", "-1", "1.", "1.5", ".5", "-.5", "1e3", "1.5e-3", "1.e5", "+.5e+2"] {
            assert!(is_number(good), "{good} should be a <number>");
        }
        for bad in ["", ".", "1e", "e3", "1..2", "1 2", "1%", "abc", "-", "+"] {
            assert!(!is_number(bad), "{bad} should not be a <number>");
        }
        assert!(is_percentage("50%"));
        assert!(is_percentage("-0.5%"));
        assert!(!is_percentage("50"));
        assert!(!is_percentage("%"));
        assert!(is_angle("45deg") && is_angle("-1turn") && is_angle("1grad") && is_angle("0rad"));
        assert!(!is_angle("45") && !is_angle("45DEG") && !is_angle("45degx"));
    }

    #[test]
    fn ws_trims_and_collapses_the_js_whitespace_set() {
        assert_eq!(ws("  a \t\n b  "), "a b");
        assert_eq!(ws("\u{feff}a\u{feff}b"), "a b", "U+FEFF is JS whitespace");
        assert_eq!(ws(""), "");
        assert_eq!(ws("   "), "");
    }

    #[test]
    fn the_string_and_ident_scanners_are_the_js_regexes() {
        for good in [r#""a""#, r#"'a'"#, r#""""#, r#""a\"b""#, r#""it's""#, r#"'say "hi"'"#] {
            assert!(is_css_string(good), "{good} should be a <string>");
        }
        for bad in [r#""a"#, r#"a"#, r#""a"b"#, r#""a\""#, r#""#, r#"'a""#] {
            assert!(!is_css_string(bad), "{bad} should not be a <string>");
        }
        for good in ["a", "-a", "_a", "a-b", "a1", "\u{4e2d}", "\\-a", "-\\1"] {
            assert!(is_css_ident(good), "{good} should be an <ident>");
        }
        for bad in ["", "-", "--a", "1a", "a b", "a.b", "-1a"] {
            assert!(!is_css_ident(bad), "{bad} should not be an <ident>");
        }
    }

    #[test]
    fn the_recorded_defaults_round_trip() {
        for d in DESCRIPTORS {
            let re = (d.parse)(d.initial);
            assert_eq!(re.as_deref(), Some(d.initial), "{} initial value must parse to itself", d.name);
        }
    }

    /// The bundle's oracle asserts a `SyntaxError` for `o.<descriptor> = "x"` on TEN descriptors;
    /// `family` takes `"x"` (it is a legal identifier); `variant` and `sizeAdjust` have no recorded
    /// case. That shared assertion is the one the grammar must not accidentally pass.
    ///
    /// `variant` IS THE ONE PLACE THIS PORT KNOWINGLY DIFFERS FROM CHROME, and it differs because the
    /// JS it ports differs. Measured, headless Chrome 150.0.7871.46 over a loopback server:
    ///
    ///     o.variant = "x"           SyntaxError          here: accepted, reads back "x"
    ///     o.variant = "none"        SyntaxError          here: accepted, reads back "none"
    ///     o.variant = "small-caps"  ok                   here: ok
    ///     o.variant = "normal"      ok                   here: ok
    ///     o.variant = "inherit"     SyntaxError          here: SyntaxError
    ///
    /// `font-variant` is a shorthand of eight longhands with a large keyword vocabulary, and the JS
    /// chose "any identifier run that is not a CSS-wide keyword" rather than transcribe it. The bundle
    /// records NO case for this descriptor, so nothing grades it in either direction, and this lane's
    /// job is to move the recorded behaviour into Rust unchanged -- tightening it is a behaviour change
    /// that wants its own recorded cases. The leniency is therefore deliberate, pinned here, and
    /// reported.
    #[test]
    fn the_ten_descriptors_the_oracle_names_reject_its_x() {
        const ORACLE_REJECTS_X: &[&str] = &[
            "style",
            "weight",
            "stretch",
            "unicodeRange",
            "featureSettings",
            "variationSettings",
            "display",
            "ascentOverride",
            "descentOverride",
            "lineGapOverride",
        ];
        for name in ORACLE_REJECTS_X {
            assert_eq!((descriptor(name).unwrap().parse)("x"), None, "{name} must reject \"x\"");
        }
        assert_eq!(parse_family("x").as_deref(), Some("x"), "`x` is a legal <family-name>");
        assert_eq!(parse_size_adjust("x"), None, "no recorded case, but the grammar has no keyword");
        assert_eq!(parse_variant("x").as_deref(), Some("x"), "the ported leniency, documented above");
        assert_eq!(parse_variant("none").as_deref(), Some("none"));
        assert_eq!(parse_variant("small-caps").as_deref(), Some("small-caps"));
        assert_eq!(parse_variant("inherit"), None, "a CSS-wide keyword is rejected here as in Chrome");
    }

    #[test]
    fn the_constructor_family_is_serialized_the_way_chrome_measured() {
        for (input, want) in [
            ("probe", "probe"),
            ("a-b", "a-b"),
            ("_x", "_x"),
            ("-x", "-x"),
            ("X", "X"),
            ("\u{e9}", "\u{e9}"),
            ("--x", "\"--x\""),
            ("default", "\"default\""),
            ("serif", "\"serif\""),
            ("sans-serif", "\"sans-serif\""),
            ("monospace", "\"monospace\""),
            ("initial", "\"initial\""),
            ("unset", "\"unset\""),
            ("revert", "\"revert\""),
            ("x\\y", "\"x\\\\y\""),
            ("x\"y", "\"x\\\"y\""),
            ("a\nb", "\"a\\a b\""),
            ("1", "\"1\""),
            ("a.b", "\"a.b\""),
            ("", "\"\""),
            ("Open Sans", "\"Open Sans\""),
            (" x ", "\" x \""),
        ] {
            assert_eq!(serialize_family(input), want, "family {input:?}");
        }
    }

    #[test]
    fn an_unclosed_final_url_is_closed_like_the_tokenizer_does() {
        assert_eq!(parse_source("url(a").as_deref(), Some("url(a)"));
        assert_eq!(parse_source("local()"), None);
        assert_eq!(parse_source("x"), None);
        assert_eq!(parse_source("url(a) format"), None);
        assert_eq!(parse_source("local(Arial Black)").as_deref(), Some("local(Arial Black)"));
        assert_eq!(parse_source("local(\"x\" y)"), None);
    }

    #[test]
    fn font_style_takes_oblique_with_up_to_two_angles() {
        assert_eq!(parse_style("normal").as_deref(), Some("normal"));
        assert_eq!(parse_style("ITALIC").as_deref(), Some("italic"));
        assert_eq!(parse_style("oblique  20deg").as_deref(), Some("oblique 20deg"));
        assert_eq!(parse_style("oblique 10deg 20deg").as_deref(), Some("oblique 10deg 20deg"));
        assert_eq!(parse_style("oblique 1deg 2deg 3deg"), None);
        assert_eq!(parse_style("oblique 20"), None);
        assert_eq!(parse_style("italic 20deg"), None);
    }

    #[test]
    fn font_weight_is_a_keyword_or_a_number_between_1_and_1000() {
        assert_eq!(parse_weight("bold").as_deref(), Some("bold"));
        assert_eq!(parse_weight("400").as_deref(), Some("400"));
        assert_eq!(parse_weight("1 1000").as_deref(), Some("1 1000"));
        assert_eq!(parse_weight("0"), None);
        assert_eq!(parse_weight("1001"), None);
        assert_eq!(parse_weight("400 500 600"), None);
        assert_eq!(parse_weight(""), None);
    }

    #[test]
    fn font_stretch_is_a_keyword_or_a_non_negative_percentage() {
        assert_eq!(parse_stretch("CONDENSED").as_deref(), Some("condensed"));
        assert_eq!(parse_stretch("50%").as_deref(), Some("50%"));
        assert_eq!(parse_stretch("50% 200%").as_deref(), Some("50% 200%"));
        assert_eq!(parse_stretch("-1%"), None);
        assert_eq!(parse_stretch("50"), None);
    }

    #[test]
    fn unicode_range_serialises_uppercase_without_leading_zeros() {
        assert_eq!(parse_unicode_range("U+0-10FFFF").as_deref(), Some("U+0-10FFFF"));
        assert_eq!(parse_unicode_range("u+0041").as_deref(), Some("U+41"));
        assert_eq!(parse_unicode_range("U+41-5a").as_deref(), Some("U+41-5A"));
        assert_eq!(parse_unicode_range("U+4??").as_deref(), Some("U+400-4FF"));
        assert_eq!(parse_unicode_range("U+41, U+5a").as_deref(), Some("U+41, U+5A"));
        assert_eq!(parse_unicode_range("U+??????"), None, "0-FFFFFF is past U+10FFFF");
        assert_eq!(parse_unicode_range("U+5a-41"), None, "an inverted range");
        assert_eq!(parse_unicode_range("U+1234567"), None);
        assert_eq!(parse_unicode_range("41"), None);
        assert_eq!(parse_unicode_range(""), None);
    }

    #[test]
    fn feature_settings_normalises_on_off_and_one() {
        assert_eq!(parse_feature_settings("NORMAL").as_deref(), Some("normal"));
        assert_eq!(parse_feature_settings(r#""liga""#).as_deref(), Some(r#""liga""#));
        assert_eq!(parse_feature_settings(r#""liga" on"#).as_deref(), Some(r#""liga""#));
        assert_eq!(parse_feature_settings(r#""liga" off"#).as_deref(), Some(r#""liga" 0"#));
        assert_eq!(parse_feature_settings(r#""liga" 1"#).as_deref(), Some(r#""liga""#));
        assert_eq!(parse_feature_settings(r#""liga" 2"#).as_deref(), Some(r#""liga" 2"#));
        assert_eq!(parse_feature_settings(r#""liga" 007"#).as_deref(), Some(r#""liga" 7"#));
        assert_eq!(parse_feature_settings(r#""liga", "kern" 2"#).as_deref(), Some(r#""liga", "kern" 2"#));
        assert_eq!(parse_feature_settings(r#""lig""#), None, "a tag is 4 characters");
        assert_eq!(parse_feature_settings("liga"), None, "a tag is a string");
        assert_eq!(parse_feature_settings(r#""liga" -1"#), None);
    }

    #[test]
    fn variation_settings_needs_a_tag_and_a_number() {
        assert_eq!(parse_variation_settings(r#""wght" 400"#).as_deref(), Some(r#""wght" 400"#));
        assert_eq!(parse_variation_settings(r#""wght" 400.50"#).as_deref(), Some(r#""wght" 400.5"#));
        assert_eq!(parse_variation_settings(r#""wght" -0"#).as_deref(), Some(r#""wght" 0"#), "JS String(-0) is \"0\"");
        assert_eq!(parse_variation_settings(r#""wght""#), None);
        assert_eq!(parse_variation_settings(r#""wght" x"#), None);
    }

    #[test]
    fn the_overrides_and_size_adjust_differ_on_normal() {
        assert_eq!(parse_override("normal").as_deref(), Some("normal"));
        assert_eq!(parse_override("90.0%").as_deref(), Some("90%"));
        assert_eq!(parse_override("-1%"), None);
        assert_eq!(parse_size_adjust("100%").as_deref(), Some("100%"));
        assert_eq!(parse_size_adjust("normal"), None, "size-adjust has no `normal`");
    }

    #[test]
    fn the_src_grammar_keeps_a_data_url_whole() {
        // The one that mattered: a naive comma split cuts `url(data:,)` in half.
        assert_eq!(split_top("url(data:,)").len(), 1);
        assert_eq!(parse_source("url(data:,)").as_deref(), Some("url(data:,)"));
        assert_eq!(parse_source(r#"url("f.woff2") format("woff2")"#).as_deref(), Some(r#"url("f.woff2") format("woff2")"#));
        assert_eq!(parse_source("URL(f.woff) TECH(variations)").as_deref(), Some("URL(f.woff) TECH(variations)"));
        assert_eq!(parse_source("local(Arial)").as_deref(), Some("local(Arial)"));
        assert_eq!(parse_source(r#"local("Helvetica Neue")"#).as_deref(), Some(r#"local("Helvetica Neue")"#));
        // Chrome 150: a string then an identifier is no `<family-name>` -- status "error".
        assert_eq!(parse_source(r#"local("x" y)"#), None, "a string and an ident do not mix");
        assert_eq!(parse_source("url(a), local(b)").as_deref(), Some("url(a), local(b)"));
        assert_eq!(parse_source("url()").as_deref(), Some("url()"), "an empty url is the bare branch matching empty");
        assert_eq!(parse_source("x"), None);
        assert_eq!(parse_source(""), None);
        // Chrome 150: the tokenizer closes the block at EOF -- status "unloaded".
        assert_eq!(parse_source("url(a").as_deref(), Some("url(a)"));
        assert_eq!(parse_source("local()"), None, "the bare branch is one-or-more");
        assert_eq!(parse_source("url(a) format"), None);
    }

    #[test]
    fn the_font_shorthand_needs_a_size_and_a_family() {
        assert_eq!(parse_font_shorthand("10px serif"), Some(vec!["serif".to_string()]));
        assert_eq!(parse_font_shorthand("bold 10px/1.2 serif"), Some(vec!["serif".to_string()]));
        assert_eq!(
            parse_font_shorthand(r#"italic small-caps bold condensed medium "My Font", serif"#),
            Some(vec![r#""My Font""#.to_string(), "serif".to_string()])
        );
        assert_eq!(parse_font_shorthand("MENU"), Some(vec!["menu".to_string()]));
        assert_eq!(parse_font_shorthand("0 serif"), Some(vec!["serif".to_string()]));
        assert_eq!(parse_font_shorthand("x"), None, "the oracle's case: no size, no family");
        assert_eq!(parse_font_shorthand("10px"), None, "a size with no family");
        assert_eq!(parse_font_shorthand("serif"), None, "a family with no size");
        assert_eq!(parse_font_shorthand(""), None);
        assert_eq!(parse_font_shorthand("a b c d e 10px serif"), None, "the prefix run is at most four");
        assert_eq!(parse_font_shorthand("10px inherit"), None, "a CSS-wide keyword is not a family");
    }
}

