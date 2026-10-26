//! `font-family` as Chrome keeps it: parsed into families, serialized back one canonical way.
//!
//! The value used to be kept exactly as written and computed `AsSpecified`, so `getComputedStyle(el)
//! .fontFamily` handed back the author's spelling -- `'x'`, `Noto   Sans`, `"title-font"` -- where
//! Chrome hands back its own serialization of the parsed list (found measuring plos;
//! `fontFamily` is one of the most-read computed properties in fingerprinting scripts). Measured in
//! Chrome 150 on `tools/realweb/seed/hs/ff.html`, inline style and computed alike:
//!
//! ```text
//!   "title-font"   -> title-font          'x'        -> x          "\41 bc"  -> Abc
//!   Noto   Sans    -> "Noto Sans"         "1abc"     -> "1abc"     "-x"      -> -x
//!   "--x"          -> "--x"               "serif"    -> "serif"    serif     -> serif
//!   "inherit"      -> "inherit"           "default"  -> "default"  "x\ty"    -> xty
//!   sans-serif,monospace -> sans-serif, monospace
//! ```
//!
//! That is Blink's `ConsumeFontFamily` + `SerializeFontFamily`: a family is a string or a run of
//! identifiers joined by single spaces (escapes resolved either way); an unquoted generic keyword is
//! itself; and a family NAME is written bare exactly when it is one CSS identifier that no keyword
//! could be mistaken for, and quoted otherwise.

use cssparser::{Parser, ParserInput, Token};

/// The generic family keywords the parser takes as keywords, not names (Blink's `ConsumeGenericFamily`).
const GENERIC: &[&str] = &[
    "serif", "sans-serif", "cursive", "fantasy", "monospace", "system-ui", "math", "fangsong",
    "ui-serif", "ui-sans-serif", "ui-monospace", "ui-rounded", "-webkit-body", "-webkit-pictograph",
];

const CSS_WIDE: &[&str] = &["initial", "inherit", "unset", "revert", "revert-layer"];

fn is_one_of(word: &str, set: &[&str]) -> bool {
    set.iter().any(|k| word.eq_ignore_ascii_case(k))
}

/// Blink's `IsCSSTokenizerIdentifier`: `-?` then a name-start code point, then name code points.
/// (`--x` is not one: the second `-` is not a name-START code point.)
fn is_tokenizer_identifier(s: &str) -> bool {
    let name_start = |c: char| c.is_ascii_alphabetic() || c == '_' || !c.is_ascii();
    let name = |c: char| name_start(c) || c.is_ascii_digit() || c == '-';
    let mut chars = s.chars().peekable();
    if chars.peek() == Some(&'-') {
        chars.next();
    }
    match chars.next() {
        Some(c) if name_start(c) => {}
        _ => return false,
    }
    chars.all(name)
}

/// One family NAME, serialized: bare when it is an identifier no keyword shadows, quoted otherwise.
fn serialize_name(name: &str) -> String {
    if is_tokenizer_identifier(name) && !is_one_of(name, GENERIC) && !is_one_of(name, CSS_WIDE) && !name.eq_ignore_ascii_case("default") {
        return name.to_string();
    }
    let mut out = String::with_capacity(name.len() + 2);
    let _ = cssparser::serialize_string(name, &mut out);
    out
}

/// The canonical text of a `font-family` value, or `None` when it is not a valid family list (which
/// makes the declaration invalid, so it declares nothing -- Chrome drops `font-family: 1abc`).
pub fn canonical(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if is_one_of(trimmed, CSS_WIDE) {
        return Some(trimmed.to_ascii_lowercase());
    }
    let mut input = ParserInput::new(trimmed);
    let mut p = Parser::new(&mut input);
    let mut families: Vec<String> = Vec::new();
    loop {
        let family = match p.next().ok()?.clone() {
            Token::QuotedString(s) => serialize_name(&s),
            Token::Ident(first) => {
                let mut words = vec![first.to_string()];
                if is_one_of(&first, GENERIC) {
                    // A generic keyword stands alone: `serif Noto` is not a family list.
                    words[0].make_ascii_lowercase();
                    families.push(words.pop()?);
                    match p.next() {
                        Err(_) => break,
                        Ok(Token::Comma) => continue,
                        Ok(_) => return None,
                    }
                }
                while let Ok(w) = p.try_parse(|p| p.expect_ident_cloned()) {
                    words.push(w.to_string());
                }
                // A lone CSS-wide keyword or `default` is not a family name; inside a longer run
                // of identifiers it is just a word (`Foo inherit` names a font).
                if words.len() == 1 && (is_one_of(&words[0], CSS_WIDE) || words[0].eq_ignore_ascii_case("default")) {
                    return None;
                }
                serialize_name(&words.join(" "))
            }
            _ => return None,
        };
        families.push(family);
        match p.next() {
            Err(_) => break,
            Ok(Token::Comma) => continue,
            Ok(_) => return None,
        }
    }
    if families.is_empty() {
        return None;
    }
    Some(families.join(", "))
}

/// An `@font-face` rule's `font-family` -- ONE `<family-name>` -- as Chrome keeps it, or `None` when it is not one: a
/// list, a generic keyword or a CSS-wide keyword drops the descriptor (Chrome 150: `font-family: serif` and `font-family:
/// a, b` leave the rule without one; `'SF Pro Display'` reads `"SF Pro Display"`, `Foo   Bar` `"Foo Bar"`, `"X"` `X`).
pub fn face_family(text: &str) -> Option<String> {
    let mut input = ParserInput::new(text.trim());
    let mut p = Parser::new(&mut input);
    let name = match p.next().ok()?.clone() {
        Token::QuotedString(s) => s.to_string(),
        Token::Ident(first) => {
            if is_one_of(&first, GENERIC) {
                return None;
            }
            let mut words = vec![first.to_string()];
            while let Ok(w) = p.try_parse(|p| p.expect_ident_cloned()) {
                words.push(w.to_string());
            }
            if words.len() == 1 && (is_one_of(&words[0], CSS_WIDE) || words[0].eq_ignore_ascii_case("default")) {
                return None;
            }
            words.join(" ")
        }
        _ => return None,
    };
    p.expect_exhausted().ok()?;
    Some(serialize_name(&name))
}

#[cfg(test)]
mod tests {
    use super::canonical;

    /// Chrome 150's answers (`tools/realweb/seed/hs/ff.html`).
    #[test]
    fn chrome_serialization() {
        let cases: &[(&str, &str)] = &[
            ("\"title-font\"", "title-font"),
            ("\"serif\"", "\"serif\""),
            ("serif", "serif"),
            ("\"Noto Sans\"", "\"Noto Sans\""),
            ("Noto Sans", "\"Noto Sans\""),
            ("Noto   Sans", "\"Noto Sans\""),
            ("'x'", "x"),
            ("\"1abc\"", "\"1abc\""),
            ("\"-x\"", "-x"),
            ("\"--x\"", "\"--x\""),
            ("\"a,b\"", "\"a,b\""),
            ("\"inherit\"", "\"inherit\""),
            ("\"default\"", "\"default\""),
            ("Arial, \"Times New Roman\", serif", "Arial, \"Times New Roman\", serif"),
            ("sans-serif,monospace", "sans-serif, monospace"),
            ("\"a\\\"b\"", "\"a\\\"b\""),
            ("\"\\41 bc\"", "Abc"),
            ("system-ui", "system-ui"),
            ("\"system-ui\"", "\"system-ui\""),
            ("\u{5ed3}", "\u{5ed3}"),
            ("\"\u{5ed3} x\"", "\"\u{5ed3} x\""),
            ("\"x\\ty\"", "xty"),
        ];
        for (input, want) in cases {
            assert_eq!(canonical(input).as_deref(), Some(*want), "{input}");
        }
    }

    #[test]
    fn invalid_lists_declare_nothing() {
        for bad in ["1abc", "serif Noto", "Arial,", ", Arial", "Arial, inherit", "default", "12px"] {
            assert_eq!(canonical(bad), None, "{bad}");
        }
        assert_eq!(canonical("INHERIT").as_deref(), Some("inherit"));
    }
}
