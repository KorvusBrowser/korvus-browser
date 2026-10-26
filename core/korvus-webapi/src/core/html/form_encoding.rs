//! The `application/x-www-form-urlencoded` serializer, with the form's submission encoding applied.
//!
//! Form submission is where the document's declared charset stops being trivia and becomes the wire
//! format. A form in a `<meta charset="euc-kr">` document submits `한` as `%C7%D1`, not `%ED%95%9C` --
//! and `<form accept-charset="shift_jis">` in that same document submits it as neither. 45 WPT files
//! across EUC-KR, EUC-JP, Shift_JIS, ISO-2022-JP and Big5 assert the bytes one code point at a time.
//!
//! Kept apart from `hyperlink.rs` because the two percent-encode sets are genuinely different and
//! conflating them is the obvious bug: the query set keeps every ASCII printable except a handful,
//! while the urlencoded set keeps only alphanumerics and `*-._`, and turns SPACE into `+`.

/// The urlencoded byte serializer's safe set: ASCII alphanumerics plus `*`, `-`, `.`, `_`. SPACE is
/// handled separately (it becomes `+`); every other byte percent-encodes.
fn urlencoded_safe(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'*' | b'-' | b'.' | b'_')
}

/// WHATWG URL's *percent-encode after encoding* with spaceAsPlus, which is what the urlencoded
/// serializer runs over each name and each value.
///
/// The unmappable branch is the same one `hyperlink.rs::encode_query` documents at length: the spec
/// appends `%26%23`, the decimal code point, `%3B` -- the already-percent-encoded spelling of `&#N;`,
/// not `&#N;` handed to the percent-encoder. And the encoder is built once and driven across the whole
/// string, because ISO-2022-JP carries an escape mode between characters.
fn encode_component(s: &str, enc: &'static encoding_rs::Encoding) -> String {
    use encoding_rs::EncoderResult;

    let mut out = String::with_capacity(s.len());
    let mut encoder = enc.new_encoder();
    let mut input = s;
    let mut buf = [0u8; 512];
    loop {
        let (result, read, written) = encoder.encode_from_utf8_without_replacement(input, &mut buf, true);
        for &b in &buf[..written] {
            if b == b' ' {
                out.push('+');
            } else if urlencoded_safe(b) {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{:02X}", b));
            }
        }
        input = &input[read..];
        match result {
            EncoderResult::InputEmpty => break,
            EncoderResult::OutputFull => continue,
            EncoderResult::Unmappable(c) => out.push_str(&format!("%26%23{}%3B", c as u32)),
        }
    }
    out
}

/// `urlencode(entries, label)` -- the "application/x-www-form-urlencoded serializer" over a form's
/// entry list.
///
/// `label` is the submission encoding's label (the form's `accept-charset`, else the document's); an
/// unrecognised label falls back to UTF-8, which is the spec's answer for a form whose accept-charset
/// names nothing supported. ISO-2022-JP and the replacement encoding are not valid OUTPUT encodings for
/// a form payload in every browser, but the WPT suite asserts ISO-2022-JP submission explicitly, so
/// they are honoured.
///
/// Called directly by `HTMLFormElement`'s `submit` row (`core/html/html_form_element.rs`). It used to be
/// reached through `op_form_urlencode`, a function BUILDER whose product `dom/08w-form-element.js` held
/// in a module const; the table's Rust body needs no such round trip.
pub(crate) fn urlencode(entries: &[(String, String)], label: &str) -> String {
    let enc = encoding_rs::Encoding::for_label(label.trim().as_bytes()).unwrap_or(encoding_rs::UTF_8);
    let mut out = String::new();
    for (name, value) in entries {
        if !out.is_empty() {
            out.push('&');
        }
        out.push_str(&encode_component(name, enc));
        out.push('=');
        out.push_str(&encode_component(value, enc));
    }
    out
}

// ── the other two form encodings ──────────────────────────────────────────────────────
//
// `enctype` picks one of three serializers, and until P13c only the first existed -- which was
// honest while nothing navigated and wrong the moment a POST carried a body. Measured against stock
// Chrome 150.0.7871.46 over loopback, a form of `<input name=q value=1><input name=r value="a b">`
// plus an EMPTY `<input type=file name=fi>`:
//
// ```text
// enctype                             Content-Type                       body
// application/x-www-form-urlencoded   (no charset parameter)             q=1&r=a+b
// text/plain                          text/plain                         "q=1\r\nr=a b\r\n"
// multipart/form-data                 multipart/form-data; boundary=...  the block below
// ```
//
// ```text
// ------WebKitFormBoundary<16>\r\n
// Content-Disposition: form-data; name="q"\r\n
// \r\n
// 1\r\n
// ------WebKitFormBoundary<16>\r\n
// Content-Disposition: form-data; name="fi"; filename=""\r\n
// Content-Type: application/octet-stream\r\n
// \r\n
// \r\n
// ------WebKitFormBoundary<16>--\r\n
// ```
//
// THE BOUNDARY IS THE WORN ENGINE'S, NOT A CONSTANT OF THIS FILE'S CHOOSING. HTML leaves the string
// to the user agent, so it is a wire tell like any other header: a body whose boundary reads
// `------------------------1a2b3c` is Firefox's and does not belong under a Chrome UA. It is
// generated here with Chromium's shape and a random suffix (`----WebKitFormBoundary` plus 16
// characters of Chromium's own alphabet). When the persona grows a request-body section this is the
// first thing that should move into it.

/// Chromium's multipart-boundary alphabet.
const BOUNDARY_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// A fresh `multipart/form-data` boundary: `----WebKitFormBoundary` and 16 characters.
pub(crate) fn multipart_boundary() -> String {
    let mut s = String::from("----WebKitFormBoundary");
    // The randomness only has to be unguessable-enough not to collide with the body; a boundary is
    // not a secret and this file owes no CSPRNG dependency for one.
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15)
        | 1;
    for _ in 0..16 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        s.push(BOUNDARY_CHARS[(seed % BOUNDARY_CHARS.len() as u64) as usize] as char);
    }
    s
}

/// HTML's "multipart/form-data encoding algorithm" over an entry list. `files` marks the entries that
/// came from a file control: an EMPTY file input still contributes a part, with `filename=""` and
/// `Content-Type: application/octet-stream`, which is what Chrome sends.
pub(crate) fn multipart_encode(entries: &[(String, String)], files: &[bool], boundary: &str) -> String {
    let parts: Vec<Option<FilePart>> = (0..entries.len())
        .map(|i| files.get(i).copied().unwrap_or(false).then(|| FilePart { name: entries[i].1.clone(), ty: String::new(), bytes: Vec::new() }))
        .collect();
    String::from_utf8_lossy(&multipart_bytes(entries, &parts, boundary)).into_owned()
}

/// A file a file control contributes to a submission: its name, its type and its bytes.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct FilePart {
    pub name: String,
    pub ty: String,
    pub bytes: Vec<u8>,
}

/// [`multipart_encode`] with each file entry's own part -- `filename` the file's name, `Content-Type` its type
/// (`application/octet-stream` when it has none), and its BYTES, which need not be text. Chrome 152 sends a picked
/// `buf.bin` of `00 01 02 61 62 63` as those six bytes under `application/octet-stream`; this sent the field's name and
/// nothing of the file. `None` is an entry's ordinary value.
pub(crate) fn multipart_bytes(entries: &[(String, String)], files: &[Option<FilePart>], boundary: &str) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    for (i, (name, value)) in entries.iter().enumerate() {
        out.extend_from_slice(b"--");
        out.extend_from_slice(boundary.as_bytes());
        out.extend_from_slice(b"\r\nContent-Disposition: form-data; name=\"");
        out.extend_from_slice(escape_field_name(name).as_bytes());
        match files.get(i).and_then(Option::as_ref) {
            Some(file) => {
                out.extend_from_slice(b"\"; filename=\"");
                out.extend_from_slice(escape_field_name(&file.name).as_bytes());
                let ty = if file.ty.is_empty() { "application/octet-stream" } else { file.ty.as_str() };
                out.extend_from_slice(format!("\"\r\nContent-Type: {ty}\r\n\r\n").as_bytes());
                out.extend_from_slice(&file.bytes);
                out.extend_from_slice(b"\r\n");
            }
            None => {
                out.extend_from_slice(b"\"\r\n\r\n");
                out.extend_from_slice(normalize_newlines(value).as_bytes());
                out.extend_from_slice(b"\r\n");
            }
        }
    }
    out.extend_from_slice(b"--");
    out.extend_from_slice(boundary.as_bytes());
    out.extend_from_slice(b"--\r\n");
    out
}

/// HTML's "escape a field name": the three characters that would break the quoted header.
fn escape_field_name(s: &str) -> String {
    s.replace('\r', "%0D").replace('\n', "%0A").replace('"', "%22")
}

/// HTML's "normalize newlines": every lone CR and lone LF becomes CRLF.
fn normalize_newlines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str("\r\n");
            }
            '\n' => out.push_str("\r\n"),
            _ => out.push(c),
        }
    }
    out
}

/// HTML's "text/plain encoding algorithm": `name=value` per entry, each followed by CRLF.
pub(crate) fn text_plain_encode(entries: &[(String, String)]) -> String {
    let mut out = String::new();
    for (name, value) in entries {
        out.push_str(&normalize_newlines(name));
        out.push('=');
        out.push_str(&normalize_newlines(value));
        out.push_str("\r\n");
    }
    out
}

#[cfg(test)]
mod p13c_tests {
    use super::*;

    #[test]
    fn the_boundary_has_chromiums_shape() {
        let b = multipart_boundary();
        assert!(b.starts_with("----WebKitFormBoundary"), "{b}");
        assert_eq!(b.len(), "----WebKitFormBoundary".len() + 16, "{b}");
        assert!(b.bytes().skip("----WebKitFormBoundary".len()).all(|c| BOUNDARY_CHARS.contains(&c)), "{b}");
    }

    #[test]
    fn multipart_is_the_block_chrome_sends() {
        let entries =
            [("q".to_string(), "1".to_string()), ("r".to_string(), "a b".to_string()), ("fi".to_string(), String::new())];
        let got = multipart_encode(&entries, &[false, false, true], "B");
        assert_eq!(
            got,
            "--B\r\nContent-Disposition: form-data; name=\"q\"\r\n\r\n1\r\n\
             --B\r\nContent-Disposition: form-data; name=\"r\"\r\n\r\na b\r\n\
             --B\r\nContent-Disposition: form-data; name=\"fi\"; filename=\"\"\r\nContent-Type: application/octet-stream\r\n\r\n\r\n\
             --B--\r\n"
        );
    }

    #[test]
    fn a_picked_file_travels_as_its_own_bytes() {
        let entries = [("q".to_string(), "1".to_string()), ("f".to_string(), "buf.bin".to_string())];
        let file = FilePart { name: "buf.bin".into(), ty: String::new(), bytes: vec![0, 1, 2, b'a', b'b', b'c'] };
        let got = multipart_bytes(&entries, &[None, Some(file)], "B");
        let mut want = b"--B\r\nContent-Disposition: form-data; name=\"q\"\r\n\r\n1\r\n\
--B\r\nContent-Disposition: form-data; name=\"f\"; filename=\"buf.bin\"\r\nContent-Type: application/octet-stream\r\n\r\n"
            .to_vec();
        want.extend_from_slice(&[0, 1, 2, b'a', b'b', b'c']);
        want.extend_from_slice(b"\r\n--B--\r\n");
        assert_eq!(got, want);
    }

    #[test]
    fn text_plain_is_name_equals_value_per_line() {
        let entries = [("q".to_string(), "1".to_string()), ("r".to_string(), "a b".to_string())];
        assert_eq!(text_plain_encode(&entries), "q=1\r\nr=a b\r\n");
    }

    #[test]
    fn a_lone_newline_in_a_value_becomes_crlf() {
        assert_eq!(normalize_newlines("a\nb\r\nc\rd"), "a\r\nb\r\nc\r\nd");
    }
}
