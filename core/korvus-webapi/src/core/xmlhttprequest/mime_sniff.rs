//! The MIME sniffing Chrome's network service does before a response's body reaches the page -- only the part an
//! `XMLHttpRequest` can see: WHEN the first bytes are handed over.
//!
//! For a sniffable type (`net::ShouldSniffMimeType`, `net/base/mime_sniffer.cc` in Chromium 150) the network service
//! holds the response back until `net::SniffMimeType` has enough content to decide, or the body ends, then delivers
//! the head and every byte so far together. Blink asks for it on every request but a `fetch()` ("Don't do mime
//! sniffing for fetch (crbug.com/2016)", `resource_loader.cc`), so an XHR sees it and a `fetch()` does not. Measured
//! on Chrome 150 with a local server: a `text/plain` body sent as 1000 bytes and, 300 ms later, 1000 more enters
//! LOADING once, with 2000 bytes, when the second part arrives; `text/html`, or `text/plain` with
//! `X-Content-Type-Options: nosniff`, enters it with the first 1000.
//!
//! The type the sniffer settles on is not ported: an HTTP response's XHR MIME type is its `Content-Type` header.

/// `net::kMaxBytesToSniff`.
const MAX_BYTES_TO_SNIFF: usize = 1024;
/// The bytes the magic-number table needs (`kBytesRequiredForMagic`).
const BYTES_REQUIRED_FOR_MAGIC: usize = 42;
/// The bytes the Office magic numbers need (`kBytesRequiredForOfficeMagic`).
const BYTES_REQUIRED_FOR_OFFICE_MAGIC: usize = 8;

/// The Office types: sniffed only to check that the body IS an Office file (`IsOfficeType`).
const OFFICE_TYPES: &[&str] = &[
    "application/msword",
    "application/vnd.ms-excel",
    "application/vnd.ms-powerpoint",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    "application/vnd.ms-excel.sheet.macroenabled.12",
    "application/vnd.ms-word.document.macroenabled.12",
    "application/vnd.ms-powerpoint.presentation.macroenabled.12",
    "application/mspowerpoint",
    "application/msexcel",
    "application/vnd.ms-word",
    "application/vnd.ms-word.document.12",
    "application/vnd.msword",
];

/// One entry of a sniffing table: a byte prefix (`.` matches any byte), compared case-insensitively when `string`
/// (`MAGIC_STRING`), under `mask` when it has one (`MAGIC_MASK`).
struct Magic {
    bytes: &'static [u8],
    string: bool,
}

const fn n(bytes: &'static [u8]) -> Magic {
    Magic { bytes, string: false }
}

const fn s(bytes: &'static [u8]) -> Magic {
    Magic { bytes, string: true }
}

impl Magic {
    /// `MatchMagicNumber`.
    fn matches(&self, content: &[u8]) -> bool {
        if content.len() < self.bytes.len() {
            return false;
        }
        if self.string {
            return content[..self.bytes.len()].eq_ignore_ascii_case(self.bytes);
        }
        self.bytes.iter().zip(content).all(|(m, c)| *m == b'.' || m == c)
    }
}

/// `kMagicNumbers`.
const MAGIC_NUMBERS: &[Magic] = &[
    n(b"%PDF-"),
    n(b"%!PS-Adobe-"),
    n(b"GIF87a"),
    n(b"GIF89a"),
    n(b"\x89PNG\x0D\x0A\x1A\x0A"),
    n(b"\xFF\xD8\xFF"),
    n(b"BM"),
    n(b"#!"),
    n(b"%!"),
    n(b"From"),
    n(b">From"),
    n(b"\x1F\x8B\x08"),
    n(b"\x2E\x52\x4D\x46"),
    n(b"\x30\x26\xB2\x75\x8E\x66\xCF\x11\xA6\xD9\x00\xAA\x00\x62\xCE\x6C"),
    n(b"I I"),
    n(b"II*"),
    n(b"MM\x00*"),
    n(b"ID3"),
    n(b"RIFF....WEBPVP"),
    n(b"\x1A\x45\xDF\xA3"),
    n(b"PK\x03\x04"),
    n(b"Rar!\x1A\x07\x00"),
    n(b"\xD7\xCD\xC6\x9A"),
    n(b"MZ"),
];

/// `kOfficeMagicNumbers`: CFB, then OOXML.
const OFFICE_MAGIC: &[Magic] = &[n(b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1"), n(b"PK\x03\x04")];

/// `kOfficeExtensionTypes`.
const OFFICE_EXTENSIONS: &[&str] = &[".doc", ".xls", ".ppt", ".docx", ".xlsx", ".pptx"];

/// The extension package magic (`SniffCRX`), looked for only in a URL ending `.crx`.
const CRX_MAGIC: &[Magic] = &[n(b"Cr24\x02\x00\x00\x00"), n(b"Cr24\x03\x00\x00\x00")];

/// `kSniffableTags`: the XML declaration, then the HTML tags.
const SNIFFABLE_TAGS: &[Magic] = &[
    n(b"<?xml"),
    s(b"<!DOCTYPE html"),
    s(b"<script"),
    s(b"<html"),
    s(b"<!--"),
    s(b"<head"),
    s(b"<iframe"),
    s(b"<h1"),
    s(b"<div"),
    s(b"<font"),
    s(b"<table"),
    s(b"<a"),
    s(b"<style"),
    s(b"<title"),
    s(b"<b"),
    s(b"<body"),
    s(b"<br"),
    s(b"<p"),
];

/// `kByteOrderMark`.
const BYTE_ORDER_MARKS: &[Magic] = &[n(b"\xFE\xFF"), n(b"\xFF\xFE"), n(b"\xEF\xBB\xBF")];

/// The MIME type Chrome reads off `headers` (`HttpResponseHeaders::GetMimeType`) -- the one implementation,
/// `transport::mime` (A/B C5): this copy read each LINE whole, so `text/html,text/plain` was one type.
fn mime_type(headers: &[(String, String)]) -> String {
    transport::mime::mime_type_and_charset(headers).mime
}

/// `IsUnknownMimeType`.
fn is_unknown(mime: &str) -> bool {
    matches!(mime, "" | "unknown/unknown" | "application/unknown" | "*/*") || !mime.contains('/')
}

/// The response's path, for the extension checks.
fn url_path(url: &str) -> String {
    url::Url::parse(url).map(|u| u.path().to_string()).unwrap_or_default()
}

/// `net::ShouldSniffMimeType` for a response from `url` with `headers`: the type to sniff against, or `None` when
/// Chrome hands the body over as it comes.
pub(crate) fn sniff_hint(url: &str, headers: &[(String, String)]) -> Option<String> {
    let scheme = url.split(':').next().unwrap_or("").to_ascii_lowercase();
    if !matches!(scheme.as_str(), "http" | "https" | "file" | "filesystem") {
        return None;
    }
    // "Only the first matching header is checked, per the fetch spec" -- its first value.
    if let Some((_, v)) = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("x-content-type-options")) {
        if v.split(',').next().unwrap_or("").trim().eq_ignore_ascii_case("nosniff") {
            return None;
        }
    }
    let mime = mime_type(headers);
    let sniffable = matches!(mime.as_str(), "text/plain" | "application/octet-stream" | "text/xml" | "application/xml") || OFFICE_TYPES.contains(&mime.as_str());
    (sniffable || is_unknown(&mime)).then_some(mime)
}

/// `TruncateStringPiece`: the first `max` bytes, and whether there were that many.
fn truncate<'a>(content: &'a [u8], max: usize, enough: &mut bool) -> &'a [u8] {
    *enough &= content.len() >= max;
    &content[..content.len().min(max)]
}

/// `LooksLikeBinary`: a byte below 0x20 other than TAB, LF, CR, FF and ESC.
fn looks_like_binary(content: &[u8]) -> bool {
    content.iter().any(|&b| b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r' | 0x0C | 0x1B))
}

/// `SniffXML`'s verdict on the first 300 bytes: decided (a tag that is not a declaration), or not yet.
fn xml_decided(content: &[u8]) -> bool {
    let mut pos = 0;
    for _ in 0..5 {
        if pos >= content.len() {
            break;
        }
        let Some(at) = content[pos..].iter().position(|&b| b == b'<') else { return false };
        pos += at;
        let current = &content[pos..];
        if s(b"<?xml").matches(current) || s(b"<!DOCTYPE").matches(current) {
            pos += 1;
            continue;
        }
        return true;
    }
    pos < content.len()
}

/// `net::SniffMimeType`'s answer to "is this enough content" for `content`, every byte of a response from `url`
/// so far, sniffed against `hint` -- false until the network service would hand the body over.
pub(crate) fn have_enough(content: &[u8], url: &str, hint: &str) -> bool {
    // An Office type is only checked to be one: 8 bytes decide it.
    if OFFICE_TYPES.contains(&hint) {
        return content.len() >= BYTES_REQUIRED_FOR_OFFICE_MAGIC;
    }
    let mut enough = true;
    let unknown = is_unknown(hint);
    let file = url.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("file:"));
    if unknown && !file {
        // HTML within the first 512 bytes, after leading whitespace.
        let c = truncate(content, 512, &mut enough);
        let start = c.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(c.len());
        if SNIFFABLE_TAGS.iter().any(|m| m.matches(&c[start..])) {
            return true;
        }
    }
    let text_plain = hint == "text/plain";
    if unknown || text_plain {
        // Binary within the first 1024 bytes: a byte-order mark says text, a control byte binary; neither -- and
        // fewer than 1024 bytes -- is not enough yet.
        let full = content.len() >= MAX_BYTES_TO_SNIFF;
        let c = &content[..content.len().min(MAX_BYTES_TO_SNIFF)];
        let binary = if BYTE_ORDER_MARKS.iter().any(|m| m.matches(c)) {
            false
        } else if looks_like_binary(c) {
            true
        } else {
            enough &= full;
            false
        };
        if !binary && text_plain {
            return enough;
        }
    }
    if hint == "text/xml" || hint == "application/xml" {
        let c = truncate(content, 300, &mut enough);
        return xml_decided(c) || enough;
    }
    let path = url_path(url);
    if path.ends_with(".crx") {
        let c = truncate(content, BYTES_REQUIRED_FOR_MAGIC, &mut enough);
        if CRX_MAGIC.iter().any(|m| m.matches(c)) {
            return true;
        }
    }
    let c = truncate(content, BYTES_REQUIRED_FOR_OFFICE_MAGIC, &mut enough);
    let lower = path.to_ascii_lowercase();
    if OFFICE_MAGIC.iter().any(|m| m.matches(c)) && OFFICE_EXTENSIONS.iter().any(|e| lower.ends_with(e)) {
        return true;
    }
    if hint == "application/octet-stream" {
        return enough;
    }
    let c = truncate(content, BYTES_REQUIRED_FOR_MAGIC, &mut enough);
    MAGIC_NUMBERS.iter().any(|m| m.matches(c)) || enough
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(ct: &str) -> Vec<(String, String)> {
        vec![("content-type".into(), ct.into())]
    }

    #[test]
    fn which_responses_are_sniffed() {
        let u = "https://a.example/x";
        assert_eq!(sniff_hint(u, &h("text/plain; charset=utf-8")).as_deref(), Some("text/plain"));
        assert_eq!(sniff_hint(u, &h("TEXT/PLAIN")).as_deref(), Some("text/plain"));
        assert_eq!(sniff_hint(u, &h("text/html")), None);
        assert_eq!(sniff_hint(u, &h("application/json")), None);
        assert_eq!(sniff_hint(u, &[]).as_deref(), Some(""));
        assert_eq!(sniff_hint(u, &h("*/*")).as_deref(), Some(""));
        assert_eq!(sniff_hint(u, &h("text")).as_deref(), Some(""));
        assert_eq!(sniff_hint(u, &h("application/octet-stream")).as_deref(), Some("application/octet-stream"));
        let mut nosniff = h("text/plain");
        nosniff.push(("X-Content-Type-Options".into(), "nosniff".into()));
        assert_eq!(sniff_hint(u, &nosniff), None);
        assert_eq!(sniff_hint("data:text/plain,x", &h("text/plain")), None);
        assert_eq!(sniff_hint("blob:https://a.example/1", &h("text/plain")), None);
    }

    /// The rows Chrome 150 was measured on (`xhr_streaming_live`'s fixture).
    #[test]
    fn when_the_sniffer_has_enough() {
        let u = "https://a.example/p";
        let x = |k: usize| vec![b'x'; k];
        assert!(!have_enough(&x(1000), u, "text/plain"));
        assert!(have_enough(&x(1024), u, "text/plain"));
        let mut bin = vec![0u8];
        bin.extend(x(99));
        assert!(have_enough(&bin, u, "text/plain"), "a binary byte decides at the magic numbers' 42");
        let mut bom = b"\xEF\xBB\xBF".to_vec();
        bom.extend(x(97));
        assert!(have_enough(&bom, u, "text/plain"));
        assert!(have_enough(b"<html><body>xxxx", u, ""));
        assert!(!have_enough(&x(100), u, ""));
        assert!(have_enough(&x(1100), u, ""));
        assert!(have_enough(&x(100), u, "application/octet-stream"));
        assert!(!have_enough(&x(7), u, "application/octet-stream"));
        assert!(have_enough(b"<?xml version='1.0'?><root>", u, "text/xml"));
        let mut decl = b"<?xml version='1.0'?>".to_vec();
        decl.extend(vec![b' '; 79]);
        assert!(!have_enough(&decl, u, "text/xml"));
        decl.extend(x(300));
        assert!(have_enough(&decl, u, "text/xml"));
        assert!(have_enough(b"GIF89a", u, ""), "a magic number decides early for an unknown type");
    }
}
