//! `mime.rs` -- a response's MIME type, read off its `Content-Type` the ways Chromium 150 reads it (A/B C5).
//!
//! A response whose ONE `Content-Type` field joins two values -- `text/html,text/html;charset=utf-8`, as allegro.pl's
//! device-check page arrived through a Google front end on 2026-10-11 -- was shown by korvus as a TEXT document (its
//! script never ran), because every reader here cut the field at its first `;` by hand and `text/html,text/html` is
//! not HTML. Chrome renders it as HTML.
//!
//! Chromium has not one reading but three, and which one a consumer gets is not a matter of taste:
//!
//! * [`mime_type_and_charset`] -- the NETWORK STACK's (`HttpResponseHeaders::GetMimeTypeAndCharset`,
//!   `net/http/http_response_headers.cc:1066`). Every `Content-Type` line is split on its commas (a coalescing
//!   header, `HttpResponseHeaders::AddHeader` :965, quote-aware `HttpUtil::ValuesIterator`), each value is parsed by
//!   `HttpUtil::ParseContentType` (`net/http/http_util.cc:112`, over `net::ParseMimeType`, `net/base/mime_util.cc:518`)
//!   and the LAST valid one wins, carrying its charset. This is `URLResponseHead::mime_type` / `charset`
//!   (`services/network/url_loader_util.cc:670`), so it is Blink's `ResourceResponse::MimeType()` and
//!   `TextEncodingName()`: what a navigation commits as (HTML, XML, the text viewer, a download), a document's
//!   `contentType` and `characterSet`, a subresource's decoding charset, a fetched `Response`'s `blob().type`
//!   (`FetchResponseData::InitFromResourceResponse`), an image's SVG test (`image_resource_content.cc:408`).
//! * [`http_content_type`] -- BLINK's `ResourceResponse::HttpContentType()` (`resource_response.cc:441`):
//!   `ExtractMIMETypeFromMediaType` (`platform/network/http_parsers.cc:613`) over the field's lines joined with
//!   ", " (`ResourceResponse::AddHttpHeaderField`), lowercased -- the FIRST value, cut at `,` or `;`. Every
//!   strict MIME check reads this one ("we want to see what the value is BEFORE content sniffing"): a classic
//!   script under `nosniff` and its blocked types (`allowed_by_nosniff.cc:178`), a module script
//!   (`module_script_fetcher.cc:70`), a style sheet's strict check and `nosniff` (`css_style_sheet_resource.cc:220`,
//!   `resource_loader.cc:1448`), and Resource Timing's `contentType` (`resource_timing_utils.cc:122`).
//! * [`extract_mime_type_from_media_type`] -- `net::ExtractMimeTypeFromMediaType` (`net/base/mime_util.cc:1011`),
//!   which `XMLHttpRequest` uses for its final MIME type (the joined field, comma-separated accepted: the first
//!   value, both halves HTTP tokens) and for `overrideMimeType`'s argument (no comma accepted).
//!
//! So `text/plain,text/html` NAVIGATES as HTML (last valid wins) while a script served `text/html,text/javascript`
//! under `nosniff` is REFUSED as `text/html` (first value) -- both measured on stock Chrome 150
//! (`core/korvus-runtime/tests/content_type_live.rs`).

/// The network stack's MIME type and charset of a response (`HttpResponseHeaders::GetMimeTypeAndCharset`): both
/// lowercase, `""` when no value named a type.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MimeAndCharset {
    pub mime: String,
    pub charset: String,
}

/// `HTTP_LWS`.
fn is_lws(c: u8) -> bool {
    c == b' ' || c == b'\t'
}

/// Every `Content-Type` value of `headers` the way the network stack stores them: each line split on the commas
/// outside a `"`-quoted string (a backslash escapes inside one), each value trimmed of `HTTP_LWS`; an empty line is
/// one empty value. Order kept.
fn content_type_values(headers: &[(String, String)]) -> Vec<&str> {
    let mut out = Vec::new();
    for (k, v) in headers {
        if !k.eq_ignore_ascii_case("content-type") {
            continue;
        }
        out.extend(split_values(v));
    }
    out
}

/// `HttpUtil::ValuesIterator(values, ',', /*ignore_empty_values=*/false)` over one line (`StringTokenizer` with `"`
/// as its quote character).
fn split_values(line: &str) -> Vec<&str> {
    let b = line.as_bytes();
    let mut out = Vec::new();
    let (mut start, mut i) = (0usize, 0usize);
    let (mut in_quote, mut in_escape) = (false, false);
    while i < b.len() {
        let c = b[i];
        if in_quote {
            if in_escape {
                in_escape = false;
            } else if c == b'\\' {
                in_escape = true;
            } else if c == b'"' {
                in_quote = false;
            }
        } else if c == b',' {
            out.push(trim_lws(&line[start..i]));
            start = i + 1;
        } else if c == b'"' {
            in_quote = true;
        }
        i += 1;
    }
    out.push(trim_lws(&line[start..]));
    out
}

fn trim_lws(s: &str) -> &str {
    s.trim_matches([' ', '\t'])
}

/// `net::ParseMimeType`: the type (as written, `None` when there is no `/` inside it) and the `(name, value)`
/// parameters. Not a validator: it does not ask for tokens, exactly as Chromium does not.
fn parse_mime_type(s: &str) -> Option<(&str, Vec<(&str, String)>)> {
    let b = s.as_bytes();
    let type_val = b.iter().position(|&c| !is_lws(c)).unwrap_or(b.len());
    let type_end = b[type_val..].iter().position(|&c| is_lws(c) || c == b';' || c == b'(').map_or(b.len(), |p| type_val + p);
    let slash = b.iter().position(|&c| c == b'/')?;
    if slash > type_end {
        return None;
    }
    let mime = &s[type_val..type_end];
    let mut params = Vec::new();
    let mut offset = b[type_end..].iter().position(|&c| c == b';').map_or(b.len(), |p| type_end + p);
    while offset < b.len() {
        offset += 1; // the ';'
        let Some(name_start) = b[offset..].iter().position(|&c| !is_lws(c)).map(|p| offset + p) else { break };
        // The name runs to the next ';' or '=', trailing spaces kept.
        let Some(eq) = b[name_start..].iter().position(|&c| c == b';' || c == b'=').map(|p| name_start + p) else { break };
        if b[eq] == b';' {
            offset = eq;
            continue;
        }
        let name = &s[name_start..eq];
        let Some(v) = b[eq + 1..].iter().position(|&c| !is_lws(c)).map(|p| eq + 1 + p) else { break };
        if b[v] == b';' {
            offset = v;
            continue;
        }
        let value;
        if b[v] != b'"' {
            let end = b[v..].iter().position(|&c| c == b';').map_or(b.len(), |p| v + p);
            value = trim_lws(&s[v..end]).trim_start_matches([' ', '\t']).to_string();
            offset = end;
        } else {
            let mut out = Vec::new();
            let mut j = v + 1;
            while j < b.len() && b[j] != b'"' {
                if b[j] == b'\\' && j + 1 < b.len() {
                    j += 1;
                }
                out.push(b[j]);
                j += 1;
            }
            value = String::from_utf8_lossy(&out).into_owned();
            offset = b[j.min(b.len())..].iter().position(|&c| c == b';').map_or(b.len(), |p| j + p);
        }
        params.push((name, value));
    }
    Some((mime, params))
}

/// `HttpUtil::ParseContentType`, one value folded into the running `(mime, charset, had_charset)`.
fn parse_content_type(value: &str, out: &mut MimeAndCharset, had_charset: &mut bool) {
    let Some((mime, params)) = parse_mime_type(value) else { return };
    // "If the server sent "*/*", it is meaningless, so do not store it."
    if value == "*/*" {
        return;
    }
    let charset = params.iter().find(|(k, _)| k.eq_ignore_ascii_case("charset")).map(|(_, v)| trim_lws(v).to_string());
    let eq = mime.eq_ignore_ascii_case(&out.mime);
    if !eq {
        out.mime = mime.to_ascii_lowercase();
    }
    // A new type drops the charset an earlier one carried; the same type again keeps it unless it names its own.
    if (!eq && *had_charset) || charset.is_some() {
        *had_charset = true;
        out.charset = charset.unwrap_or_default().to_ascii_lowercase();
    }
}

/// The network stack's MIME type and charset of a response with `headers` (every `Content-Type` line).
pub fn mime_type_and_charset(headers: &[(String, String)]) -> MimeAndCharset {
    let mut out = MimeAndCharset::default();
    let mut had_charset = false;
    for v in content_type_values(headers) {
        parse_content_type(v, &mut out, &mut had_charset);
    }
    out
}

/// [`mime_type_and_charset`] of ONE `Content-Type` value (a caller that kept only the field, not the headers).
pub fn mime_type_and_charset_of(value: &str) -> MimeAndCharset {
    mime_type_and_charset(&[("content-type".to_string(), value.to_string())])
}

/// The `Content-Type` field as Blink's `ResourceResponse` holds it: every line, joined with ", " (`None`: no line).
pub fn joined_content_type(headers: &[(String, String)]) -> Option<String> {
    let mut out: Option<String> = None;
    for (k, v) in headers {
        if k.eq_ignore_ascii_case("content-type") {
            out = Some(match out {
                None => v.clone(),
                Some(o) => format!("{o}, {v}"),
            });
        }
    }
    out
}

/// Blink's `ExtractMIMETypeFromMediaType`: from the first non-tab/space character to the first `,` or `;`, trailing
/// tabs and spaces cut. Case kept; a value of only tabs and spaces comes back whole.
pub fn blink_extract_mime_type(media_type: &str) -> &str {
    let b = media_type.as_bytes();
    let Some(start) = b.iter().position(|&c| !is_lws(c)) else { return media_type };
    let mut end = start;
    for (i, &c) in b.iter().enumerate().skip(start) {
        if c == b',' || c == b';' {
            break;
        }
        if !is_lws(c) {
            end = i + 1;
        }
    }
    &media_type[start..end]
}

/// Blink's `ResourceResponse::HttpContentType()`: the joined field lowercased, then [`blink_extract_mime_type`] --
/// `None` when the response has no `Content-Type` at all (Blink's null string), `Some("")` for an empty one.
pub fn http_content_type(headers: &[(String, String)]) -> Option<String> {
    let joined = joined_content_type(headers)?.to_ascii_lowercase();
    Some(blink_extract_mime_type(&joined).to_string())
}

/// `X-Content-Type-Options: nosniff` as Blink reads it (`ParseContentTypeOptionsHeader`, `http_parsers.cc`): the
/// field's lines joined with ", ", its first comma-separated entry trimmed of tabs and spaces, `nosniff` in any case.
pub fn nosniff(headers: &[(String, String)]) -> bool {
    let mut joined: Option<String> = None;
    for (k, v) in headers {
        if k.eq_ignore_ascii_case("x-content-type-options") {
            joined = Some(match joined {
                None => v.clone(),
                Some(o) => format!("{o}, {v}"),
            });
        }
    }
    joined.is_some_and(|v| trim_lws(v.split(',').next().unwrap_or("")).eq_ignore_ascii_case("nosniff"))
}

/// An RFC 9110 token (`HttpUtil::IsToken`): non-empty, every byte a `tchar`.
pub fn is_http_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(b, b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~')
        })
}

/// `net::ExtractMimeTypeFromMediaType`: the `type/subtype` before the first `;` (and the first `,` when
/// `accept_comma_separated`), exactly one `/`, the type's leading and the subtype's trailing ASCII whitespace cut,
/// both halves tokens. Case kept.
pub fn extract_mime_type_from_media_type(s: &str, accept_comma_separated: bool) -> Option<String> {
    let mut end = s.find(';').unwrap_or(s.len());
    if accept_comma_separated {
        end = end.min(s.find(',').unwrap_or(s.len()));
    }
    let mut parts = s[..end].split('/');
    let (top, sub) = (parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let ws = |c: char| matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r');
    let (top, sub) = (top.trim_start_matches(ws), sub.trim_end_matches(ws));
    (is_http_token(top) && is_http_token(sub)).then(|| format!("{top}/{sub}"))
}

/// The `charset` parameter of a media type as `XMLHttpRequest` reads an override's (Blink's
/// `ExtractCharsetFromMediaType` / `FindCharsetInMediaType`, `xml_http_request.cc:123`): `""` for none.
pub fn charset_param(media_type: &str) -> String {
    let b = media_type.as_bytes();
    let lower = media_type.to_ascii_lowercase();
    let len = b.len();
    let mut pos = 0usize;
    let mut found = false;
    while pos < len {
        let Some(at) = lower[pos..].find("charset").map(|i| pos + i) else { return String::new() };
        // "Give up if we find "charset" at the head."
        if at == 0 {
            return String::new();
        }
        // Not a substring of some longer word.
        if b[at - 1] > b' ' && b[at - 1] != b';' {
            pos = at + 7;
            continue;
        }
        pos = at + 7;
        while pos < len && b[pos] <= b' ' {
            pos += 1;
        }
        let eq = pos < len && b[pos] == b'=';
        pos += 1;
        if eq {
            found = true;
            break;
        }
    }
    if !found {
        return String::new();
    }
    while pos < len && (b[pos] <= b' ' || b[pos] == b'"' || b[pos] == b'\'') {
        pos += 1;
    }
    let start = pos;
    while pos < len && b[pos] > b' ' && b[pos] != b'"' && b[pos] != b'\'' && b[pos] != b';' {
        pos += 1;
    }
    String::from_utf8_lossy(&b[start.min(len)..pos.min(len)]).into_owned()
}

/// Blink's `ParsedContentType(value).IsValid()` (`platform/network/parsed_content_type.cc`, `kNormal` mode): a
/// token, `/`, a token, then only `; name=value` parameters (value a token or a quoted string), optional
/// whitespace between. `XMLHttpRequest.overrideMimeType` keeps its argument only if this holds.
pub fn parsed_content_type_is_valid(value: &str) -> bool {
    fn tchar(c: u8) -> bool {
        c >= 0x20 && c < 0x80 && !matches!(c, b' ' | b';' | b'"' | b'(' | b')' | b'<' | b'>' | b'@' | b',' | b':' | b'\\' | b'/' | b'[' | b']' | b'?' | b'=')
    }
    let b = value.as_bytes();
    let mut i = 0usize;
    let skip_ws = |i: &mut usize| {
        while *i < b.len() && (b[*i] == b' ' || b[*i] == b'\t') {
            *i += 1;
        }
    };
    let token = |i: &mut usize| -> bool {
        let start = *i;
        while *i < b.len() && tchar(b[*i]) {
            *i += 1;
        }
        start != *i
    };
    let consume = |i: &mut usize, c: u8| -> bool {
        if *i < b.len() && b[*i] == c {
            *i += 1;
            true
        } else {
            false
        }
    };
    skip_ws(&mut i);
    if !token(&mut i) {
        return false;
    }
    skip_ws(&mut i);
    if !consume(&mut i, b'/') {
        return false;
    }
    skip_ws(&mut i);
    if !token(&mut i) {
        return false;
    }
    skip_ws(&mut i);
    while i < b.len() {
        if !consume(&mut i, b';') {
            return false;
        }
        skip_ws(&mut i);
        if !token(&mut i) {
            return false;
        }
        skip_ws(&mut i);
        if !consume(&mut i, b'=') {
            return false;
        }
        skip_ws(&mut i);
        if i >= b.len() {
            return false;
        }
        if b[i] == b'"' {
            i += 1;
            loop {
                if i >= b.len() {
                    return false;
                }
                if b[i] == b'"' {
                    i += 1;
                    break;
                }
                if b[i] == b'\\' {
                    i += 1;
                    if i >= b.len() {
                        return false;
                    }
                }
                i += 1;
            }
        } else if !token(&mut i) {
            return false;
        }
        skip_ws(&mut i);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ct(lines: &[&str]) -> Vec<(String, String)> {
        lines.iter().map(|v| ("Content-Type".to_string(), v.to_string())).collect()
    }

    fn net(lines: &[&str]) -> (String, String) {
        let m = mime_type_and_charset(&ct(lines));
        (m.mime, m.charset)
    }

    #[test]
    fn the_network_stack_keeps_the_last_valid_type_and_its_charset() {
        let p = |a: &str, b: &str| (a.to_string(), b.to_string());
        assert_eq!(net(&["text/html,text/html;charset=utf-8"]), p("text/html", "utf-8"));
        assert_eq!(net(&["text/html", "text/html;charset=utf-8"]), p("text/html", "utf-8"));
        assert_eq!(net(&["text/plain,text/html"]), p("text/html", ""));
        assert_eq!(net(&["text/html;charset=gbk,text/plain"]), p("text/plain", ""));
        assert_eq!(net(&["text/html;charset=gbk,text/html"]), p("text/html", "gbk"));
        assert_eq!(net(&["text/html,bogus"]), p("text/html", ""));
        assert_eq!(net(&["bogus,text/html"]), p("text/html", ""));
        assert_eq!(net(&["text/html,*/*"]), p("text/html", ""));
        assert_eq!(net(&["*/*"]), p("", ""));
        assert_eq!(net(&[""]), p("", ""));
        assert_eq!(net(&["TEXT/HTML; Charset=\"Shift_JIS\""]), p("text/html", "shift_jis"));
        // a comma inside a quoted parameter does not split the value
        assert_eq!(net(&["text/html;charset=\"a,b\""]), p("text/html", "a,b"));
        assert_eq!(net(&[]), p("", ""));
    }

    #[test]
    fn blink_reads_the_first_value_of_the_joined_field() {
        assert_eq!(http_content_type(&ct(&["text/html,text/javascript"])).as_deref(), Some("text/html"));
        assert_eq!(http_content_type(&ct(&["Text/JavaScript ; charset=x", "text/html"])).as_deref(), Some("text/javascript"));
        assert_eq!(http_content_type(&ct(&["", "text/css"])).as_deref(), Some(""));
        assert_eq!(http_content_type(&[]), None);
    }

    #[test]
    fn xhr_extracts_a_tokenised_type() {
        assert_eq!(extract_mime_type_from_media_type("text/plain,text/html", true).as_deref(), Some("text/plain"));
        assert_eq!(extract_mime_type_from_media_type("text/plain,text/html", false), None);
        assert_eq!(extract_mime_type_from_media_type(" Text/HTML ;x", false).as_deref(), Some("Text/HTML"));
        assert_eq!(extract_mime_type_from_media_type("text/json/json", false), None);
        assert_eq!(extract_mime_type_from_media_type("", true), None);
        assert_eq!(charset_param("text/plain; charset=windows-1252"), "windows-1252");
        assert_eq!(charset_param("text/plain"), "");
    }
}
