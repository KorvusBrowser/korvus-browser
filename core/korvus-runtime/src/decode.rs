//! `decode.rs` -- a document's bytes to its text: the HTML encoding sniff, then the decode.
//!
//! # What was wrong
//!
//! The navigation paths handed the engine `FetchResponse::body`, a `String` made by
//! `String::from_utf8_lossy` over the bytes. Every byte sequence that is not UTF-8 became U+FFFD before
//! the parser saw it, while `navigation.rs::meta_encoding` still read the `<meta charset>` off the parsed
//! tree -- so `document.characterSet` said `EUC-KR` and the text said `���� ���`. L19 E12, measured
//! against stock Chrome 150.0.7871.46 over loopback (`korvus` = master `fd6473b8`):
//!
//! ```text
//! fixture                                          chrome 150                    korvus before
//! <meta http-equiv=Content-Type ... EUC-KR>        EUC-KR     '문서 모듈'        EUC-KR  '���� ���'
//! latin-1 bytes, no declaration                    windows-1252 'português'      UTF-8   'portugu�s'
//! <meta charset=iso-8859-1>                        windows-1252 'português'      windows-1252 'portugu�s'
//! Content-Type: text/html; charset=Shift_JIS       Shift_JIS  '日本語テキスト'   UTF-8   mojibake
//! header gbk + <meta charset=utf-8>                GBK (header wins)             UTF-8   mojibake
//! UTF-8 BOM + header windows-1252                  UTF-8 (BOM wins)              UTF-8
//! UTF-16LE BOM, no declaration                     UTF-16LE   '한국어'           UTF-8   the raw markup as text
//! <meta charset=euc-kr> after 1100 bytes (head)    EUC-KR                        EUC-KR  mojibake
//! ... after 1100 bytes, inside <body>              EUC-KR                        -
//! ... after 1200 bytes of <p> text                 EUC-KR                        -
//! ... after a 3000-byte <script> in <head>         EUC-KR                        -
//! header charset=bogus + <meta charset=euc-kr>     EUC-KR (a bad label is none)  -
//! <meta charset=utf-16le> over UTF-8 bytes         UTF-8                         -
//! header charset=utf-8 over latin-1 bytes          UTF-8 'portugu�s'             -
//! http-equiv content-type Shift_JIS in <head>      Shift_JIS                     -
//! ```
//!
//! So the order is the spec's -- BOM, then the transport's `charset`, then a `<meta>` declaration -- with
//! one measured difference: Chrome's scan for the `<meta>` is not bounded by the spec prescan's 1024
//! bytes. It honours a declaration anywhere in the markup, which is what its tokenizer-driven
//! `HTMLMetaCharsetParser` plus the parser's own late "change the encoding" amount to. This scan
//! therefore runs over the whole buffer and steps over raw-text elements (`<script>`, `<style>`, ...)
//! and comments, where a tokenizer would not see a tag.
//!
//! # A document that declares nothing
//!
//! Chrome runs its statistical detector (CED) with the UI locale as a hint and never detects UTF-8 for a
//! page off the network. Its answers, measured under `LANG=ko_KR.UTF-8` and `LANG=en_US.UTF-8`:
//!
//! ```text
//! bytes                                   ko_KR          en_US
//! ASCII only                              windows-1252   windows-1252
//! latin-1 'português'                     windows-1252   windows-1252
//! UTF-8 'português café'                  EUC-KR         windows-1252
//! UTF-8 '한국어 português'                EUC-KR         windows-1252
//! EUC-KR Korean prose                     EUC-KR         EUC-KR
//! Shift_JIS Japanese prose                Shift_JIS      Shift_JIS
//! ```
//!
//! There is no CED in this tree, and this is NOT one. [`detect`] is a small rule set that reproduces
//! every row above and says how: ASCII is windows-1252; UTF-8-valid bytes are the ambiguous case CED
//! resolves to the locale's default; high bytes that stand alone between ASCII are a single-byte legacy
//! page; otherwise the first East Asian double-byte encoding (the locale's own first) that decodes the
//! bytes without an error. A page on which CED's statistics would choose differently is a row this
//! rule gets wrong -- which is a counted `thin`, not a hidden one. The locale is the PERSONA's first
//! language (the device the page is told it runs on), mapped through the HTML spec's
//! suggested-default-encoding table.

use encoding_rs::Encoding;

/// How [`sniff`] decided, for a test or a log to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    Bom,
    Transport,
    Meta,
    Detected,
}

/// The decoded document: its text and the encoding `document.characterSet` reports.
pub struct Decoded {
    pub text: String,
    pub encoding: &'static Encoding,
    pub confidence: Confidence,
}

/// Decode a navigation response's `bytes` into the text the parser reads.
///
/// `charset` is the transport layer's declaration: the network stack's charset of the response, the one
/// the WINNING `Content-Type` value carried (`transport::mime::mime_type_and_charset`, A/B C5); `is_xml`
/// selects XML's rules (no `<meta>`, UTF-8 by default); `locale` is a BCP 47 tag for the no-declaration
/// default.
pub fn decode(bytes: &[u8], charset: Option<&str>, is_xml: bool, locale: &str) -> Decoded {
    let (encoding, confidence, bom_len) = sniff(bytes, charset, is_xml, locale);
    let body = &bytes[bom_len..];
    // No BOM handling here: a BOM was consumed above, and one that disagrees with the declaration is
    // content, exactly as the spec's "decode without BOM" treats it once the sniff has run.
    let (text, _) = encoding.decode_without_bom_handling(body);
    Decoded { text: text.into_owned(), encoding, confidence }
}

/// The encoding sniff: `(encoding, how, bytes of BOM to skip)`.
pub fn sniff(bytes: &[u8], charset: Option<&str>, is_xml: bool, locale: &str) -> (&'static Encoding, Confidence, usize) {
    if let Some((e, n)) = Encoding::for_bom(bytes) {
        return (e, Confidence::Bom, n);
    }
    if let Some(e) = charset.and_then(transport_charset) {
        return (e, Confidence::Transport, 0);
    }
    if is_xml {
        return (encoding_rs::UTF_8, Confidence::Detected, 0);
    }
    if let Some(e) = meta_declaration(bytes) {
        return (e, Confidence::Meta, 0);
    }
    (detect(bytes, locale), Confidence::Detected, 0)
}

/// The transport's charset label, when it names an encoding. A label no encoding answers to is no
/// declaration at all (measured: `charset=bogus` falls through to the `<meta>`). WHICH `charset` parameter
/// it is -- one `Content-Type` can carry several values -- is the network stack's choice
/// (`transport::mime::mime_type_and_charset`), not a split here (A/B C5).
pub fn transport_charset(charset: &str) -> Option<&'static Encoding> {
    if charset.is_empty() {
        return None;
    }
    Encoding::for_label(charset.as_bytes())
}

fn is_ws(b: u8) -> bool {
    matches!(b, b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

/// Elements whose content a tokenizer reads as text, so a `<meta` inside one is not a tag.
const RAW_TEXT: &[&[u8]] = &[b"script", b"style", b"textarea", b"title", b"xmp", b"iframe", b"noembed", b"noframes"];

/// The first `<meta>` encoding declaration in `bytes`: WHATWG HTML "prescan a byte stream to determine
/// its encoding", run over the whole buffer (see the module header for why not 1024 bytes), stepping
/// over raw-text elements.
pub fn meta_declaration(bytes: &[u8]) -> Option<&'static Encoding> {
    let n = bytes.len();
    let mut i = 0usize;
    while i < n {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let rest = &bytes[i..];
        if rest.starts_with(b"<!--") {
            i = find(bytes, i + 4, b"-->").map(|p| p + 3).unwrap_or(n);
            continue;
        }
        if rest.len() > 5 && rest[..5].eq_ignore_ascii_case(b"<meta") && (is_ws(rest[5]) || rest[5] == b'/') {
            i += 5;
            if let Some(e) = meta_element(bytes, &mut i) {
                return Some(e);
            }
            continue;
        }
        let letter_at = |j: usize| bytes.get(j).is_some_and(|b| b.is_ascii_alphabetic());
        if letter_at(i + 1) || (bytes.get(i + 1) == Some(&b'/') && letter_at(i + 2)) {
            let closing = bytes[i + 1] == b'/';
            let start = i + if closing { 2 } else { 1 };
            let mut j = start;
            while j < n && !is_ws(bytes[j]) && bytes[j] != b'>' && bytes[j] != b'/' {
                j += 1;
            }
            let name = bytes[start..j].to_ascii_lowercase();
            i = j;
            while get_attribute(bytes, &mut i).is_some() {}
            if !closing && RAW_TEXT.iter().any(|t| *t == name.as_slice()) {
                // Everything up to the matching end tag is text.
                let mut end = Vec::with_capacity(name.len() + 2);
                end.extend_from_slice(b"</");
                end.extend_from_slice(&name);
                i = find_ci(bytes, i, &end).unwrap_or(n);
            }
            continue;
        }
        if rest.starts_with(b"<!") || rest.starts_with(b"</") || rest.starts_with(b"<?") {
            i = find(bytes, i + 2, b">").map(|p| p + 1).unwrap_or(n);
            continue;
        }
        i += 1;
    }
    None
}

/// The attribute loop of the prescan's `<meta` branch. `i` is just past `<meta`.
fn meta_element(bytes: &[u8], i: &mut usize) -> Option<&'static Encoding> {
    let mut seen: Vec<Vec<u8>> = Vec::new();
    let mut got_pragma = false;
    let mut need_pragma: Option<bool> = None;
    let mut charset: Option<Option<&'static Encoding>> = None;
    while let Some((name, value)) = get_attribute(bytes, i) {
        if seen.contains(&name) {
            continue;
        }
        seen.push(name.clone());
        match name.as_slice() {
            b"http-equiv" => {
                if value.eq_ignore_ascii_case(b"content-type") {
                    got_pragma = true;
                }
            }
            b"content" => {
                if charset.is_none() {
                    if let Some(label) = charset_from_content(&value) {
                        charset = Some(Encoding::for_label(&label));
                        need_pragma = Some(true);
                    }
                }
            }
            b"charset" => {
                charset = Some(Encoding::for_label(&value));
                need_pragma = Some(false);
            }
            _ => {}
        }
    }
    let need = need_pragma?;
    if need && !got_pragma {
        return None;
    }
    let e = charset.flatten()?;
    // A UTF-16 declaration in bytes an ASCII-compatible scan could read is a lie about those bytes.
    if e == encoding_rs::UTF_16BE || e == encoding_rs::UTF_16LE {
        return Some(encoding_rs::UTF_8);
    }
    if e == encoding_rs::X_USER_DEFINED {
        return Some(encoding_rs::WINDOWS_1252);
    }
    Some(e)
}

/// WHATWG "get an attribute": `(lowercased name, value)`, or `None` at `>` / end of input.
fn get_attribute(bytes: &[u8], i: &mut usize) -> Option<(Vec<u8>, Vec<u8>)> {
    let n = bytes.len();
    while *i < n && (is_ws(bytes[*i]) || bytes[*i] == b'/') {
        *i += 1;
    }
    if *i >= n || bytes[*i] == b'>' {
        return None;
    }
    let mut name = Vec::new();
    let mut value = Vec::new();
    loop {
        if *i >= n {
            return Some((name, value));
        }
        let c = bytes[*i];
        if c == b'=' && !name.is_empty() {
            *i += 1;
            break;
        }
        if is_ws(c) {
            while *i < n && is_ws(bytes[*i]) {
                *i += 1;
            }
            if *i < n && bytes[*i] == b'=' {
                *i += 1;
                break;
            }
            return Some((name, value));
        }
        if c == b'/' || c == b'>' {
            return Some((name, value));
        }
        name.push(c.to_ascii_lowercase());
        *i += 1;
    }
    while *i < n && is_ws(bytes[*i]) {
        *i += 1;
    }
    if *i >= n {
        return Some((name, value));
    }
    let c = bytes[*i];
    if c == b'"' || c == b'\'' {
        *i += 1;
        while *i < n && bytes[*i] != c {
            value.push(bytes[*i].to_ascii_lowercase());
            *i += 1;
        }
        *i += 1;
        return Some((name, value));
    }
    if c == b'>' {
        return Some((name, value));
    }
    while *i < n && !is_ws(bytes[*i]) && bytes[*i] != b'>' {
        value.push(bytes[*i].to_ascii_lowercase());
        *i += 1;
    }
    Some((name, value))
}

/// WHATWG "extract a character encoding from a meta element": the label after `charset=` in a
/// `content` value.
pub(crate) fn charset_from_content(content: &[u8]) -> Option<Vec<u8>> {
    let n = content.len();
    let mut pos = 0usize;
    loop {
        let at = find_ci(content, pos, b"charset")?;
        let mut j = at + 7;
        while j < n && is_ws(content[j]) {
            j += 1;
        }
        if j >= n || content[j] != b'=' {
            pos = j;
            continue;
        }
        j += 1;
        while j < n && is_ws(content[j]) {
            j += 1;
        }
        if j >= n {
            return None;
        }
        let q = content[j];
        if q == b'"' || q == b'\'' {
            let end = content[j + 1..].iter().position(|&b| b == q)?;
            return Some(content[j + 1..j + 1 + end].to_vec());
        }
        let end = content[j..].iter().position(|&b| b == b';' || is_ws(b)).map(|p| j + p).unwrap_or(n);
        return Some(content[j..end].to_vec());
    }
}

fn find(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from > hay.len() {
        return None;
    }
    hay[from..].windows(needle.len()).position(|w| w == needle).map(|p| from + p)
}

fn find_ci(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from > hay.len() {
        return None;
    }
    hay[from..].windows(needle.len()).position(|w| w.eq_ignore_ascii_case(needle)).map(|p| from + p)
}

/// The HTML spec's "suggested default encoding" for a locale (§13.2.3.3's table), keyed by the
/// language tag's primary subtag, with the two Chinese scripts told apart by region.
pub fn locale_default(locale: &str) -> &'static Encoding {
    let lower = locale.to_ascii_lowercase().replace('_', "-");
    let mut parts = lower.split('-');
    let lang = parts.next().unwrap_or("");
    let rest: Vec<&str> = parts.collect();
    let label: &str = match lang {
        "ar" | "fa" | "ur" => "windows-1256",
        "ba" | "be" | "bg" | "kk" | "ky" | "mk" | "ru" | "tt" | "uk" => "windows-1251",
        "sr" if !rest.contains(&"latn") => "windows-1251",
        "cs" | "hr" | "hu" | "pl" | "ro" | "sk" | "sl" | "sr" => "windows-1250",
        "el" => "iso-8859-7",
        "et" | "lt" | "lv" => "windows-1257",
        "he" => "windows-1255",
        "ja" => "shift_jis",
        "ko" => "euc-kr",
        "ku" | "tr" | "az" => "windows-1254",
        "th" => "windows-874",
        "vi" => "windows-1258",
        "zh" if rest.iter().any(|r| matches!(*r, "tw" | "hk" | "mo" | "hant")) => "big5",
        "zh" => "gbk",
        _ => "windows-1252",
    };
    Encoding::for_label(label.as_bytes()).unwrap_or(encoding_rs::WINDOWS_1252)
}

/// The encoding of a document that declared none. See the module header: a measured rule set, not a
/// statistical detector.
pub fn detect(bytes: &[u8], locale: &str) -> &'static Encoding {
    if bytes.is_ascii() {
        return encoding_rs::WINDOWS_1252;
    }
    let default = locale_default(locale);
    if std::str::from_utf8(bytes).is_ok() {
        return default;
    }
    let isolated = bytes.iter().enumerate().all(|(i, &b)| {
        b < 0x80 || ((i == 0 || bytes[i - 1] < 0x80) && bytes.get(i + 1).is_none_or(|&n| n < 0x80))
    });
    if isolated {
        return if is_double_byte(default) { encoding_rs::WINDOWS_1252 } else { default };
    }
    let mut order: Vec<&'static Encoding> = Vec::new();
    if is_double_byte(default) {
        order.push(default);
    }
    for e in [encoding_rs::EUC_KR, encoding_rs::SHIFT_JIS, encoding_rs::EUC_JP, encoding_rs::BIG5, encoding_rs::GBK] {
        if !order.contains(&e) {
            order.push(e);
        }
    }
    for e in order {
        let (_, had_errors) = e.decode_without_bom_handling(bytes);
        if !had_errors {
            return e;
        }
    }
    default
}

fn is_double_byte(e: &'static Encoding) -> bool {
    [encoding_rs::EUC_KR, encoding_rs::SHIFT_JIS, encoding_rs::EUC_JP, encoding_rs::BIG5, encoding_rs::GBK].contains(&e)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(s: &str, e: &'static Encoding) -> Vec<u8> {
        e.encode(s).0.into_owned()
    }

    /// Every row of the module header's first table, as the sniff sees it.
    #[test]
    fn declarations_in_chromes_order() {
        let euckr = |s: &str| enc(s, encoding_rs::EUC_KR);
        let case = |bytes: &[u8], ct: &str| {
            let cs = transport::mime::mime_type_and_charset_of(ct).charset;
            let d = decode(bytes, Some(&cs), false, "ko-KR");
            (d.encoding.name(), d.text)
        };
        let mut b = br#"<!doctype html><html><head><meta http-equiv="Content-Type" content="text/html; charset=EUC-KR"><title>"#.to_vec();
        b.extend(euckr("문서 모듈"));
        assert_eq!(case(&b, "text/html"), ("EUC-KR", r#"<!doctype html><html><head><meta http-equiv="Content-Type" content="text/html; charset=EUC-KR"><title>문서 모듈"#.to_string()));

        let b = enc(r#"<!doctype html><meta charset="iso-8859-1"><p>português"#, encoding_rs::WINDOWS_1252);
        assert_eq!(case(&b, "text/html").0, "windows-1252");
        assert!(case(&b, "text/html").1.ends_with("português"));

        let b = enc("<p>日本語テキスト", encoding_rs::SHIFT_JIS);
        assert_eq!(case(&b, "text/html; charset=Shift_JIS"), ("Shift_JIS", "<p>日本語テキスト".to_string()));

        let b = enc(r#"<meta charset="utf-8"><p>中文字符"#, encoding_rs::GBK);
        assert_eq!(case(&b, "text/html; charset=gbk").0, "GBK");

        let mut b = b"\xef\xbb\xbf".to_vec();
        b.extend("<p>português 한".as_bytes());
        assert_eq!(case(&b, "text/html; charset=windows-1252"), ("UTF-8", "<p>português 한".to_string()));

        let mut b = b"\xff\xfe".to_vec();
        for u in "<p>한국어".encode_utf16() {
            b.extend(u.to_le_bytes());
        }
        assert_eq!(case(&b, "text/html"), ("UTF-16LE", "<p>한국어".to_string()));

        let mut b = format!("<title>lm</title><!--{}--><meta charset=\"euc-kr\"><p>", "x".repeat(1100)).into_bytes();
        b.extend(euckr("문서"));
        assert_eq!(case(&b, "text/html").0, "EUC-KR");
        let mut b = format!("<title>tl</title><p>{}</p><meta charset=\"euc-kr\"><p>", "x ".repeat(600)).into_bytes();
        b.extend(euckr("문서"));
        assert_eq!(case(&b, "text/html").0, "EUC-KR");
        let mut b = format!("<head><title>sl</title><script>var a=\"{}\"</script><meta charset=\"euc-kr\"></head><p>", "x".repeat(3000)).into_bytes();
        b.extend(euckr("문서"));
        assert_eq!(case(&b, "text/html").0, "EUC-KR");

        let b = enc(r#"<meta charset="euc-kr"><p>문서"#, encoding_rs::EUC_KR);
        assert_eq!(case(&b, "text/html; charset=bogus").0, "EUC-KR");
        assert_eq!(case(r#"<meta charset="utf-16le"><p>한국어"#.as_bytes(), "text/html").0, "UTF-8");
        let b = enc("<p>português", encoding_rs::WINDOWS_1252);
        assert_eq!(case(&b, "text/html; charset=utf-8"), ("UTF-8", "<p>portugu\u{fffd}s".to_string()));
        let b = enc(r#"<head><meta http-equiv="Content-Type" content="text/html; charset=shift_jis"></head><p>日本語"#, encoding_rs::SHIFT_JIS);
        assert_eq!(case(&b, "text/html").0, "Shift_JIS");
    }

    /// A `<meta` a tokenizer reads as text is not a declaration, and neither is one missing its pragma.
    #[test]
    fn text_that_looks_like_a_meta_is_not_one() {
        assert_eq!(meta_declaration(br#"<script>"<meta charset=euc-kr>"</script>"#), None);
        assert_eq!(meta_declaration(br#"<!-- <meta charset=euc-kr> -->"#), None);
        assert_eq!(meta_declaration(br#"<meta content="text/html; charset=euc-kr">"#), None);
        assert_eq!(meta_declaration(br#"<meta content="text/html; charset=euc-kr" http-equiv=content-type>"#), Some(encoding_rs::EUC_KR));
        assert_eq!(meta_declaration(br#"<p title="<meta charset=euc-kr>">"#), None);
    }

    /// The module header's second table, both locales.
    #[test]
    fn no_declaration_matches_the_measured_detector_answers() {
        let rows: Vec<(Vec<u8>, &str, &str)> = vec![
            (b"<p>plain ascii".to_vec(), "windows-1252", "windows-1252"),
            (enc("<p>português", encoding_rs::WINDOWS_1252), "windows-1252", "windows-1252"),
            ("<p>português café".as_bytes().to_vec(), "EUC-KR", "windows-1252"),
            ("<p>한국어 português".as_bytes().to_vec(), "EUC-KR", "windows-1252"),
            (enc("<p>대한민국 문서 모듈 한국어 텍스트입니다", encoding_rs::EUC_KR), "EUC-KR", "EUC-KR"),
            (enc("<p>日本語のテキストです。これはテストです。", encoding_rs::SHIFT_JIS), "Shift_JIS", "Shift_JIS"),
        ];
        for (bytes, ko, en) in rows {
            assert_eq!(detect(&bytes, "ko-KR").name(), ko, "{:?}", String::from_utf8_lossy(&bytes));
            assert_eq!(detect(&bytes, "en-US").name(), en, "{:?}", String::from_utf8_lossy(&bytes));
        }
    }
}
