//! `text_viewer.rs` -- what a navigation to a NON-HTML, non-XML response becomes.
//!
//! Chrome does not run a text file, a script, a style sheet, a JSON body or an image through the HTML
//! parser. It builds a document of its own around the bytes, and that document is what a page, a probe
//! and a text extractor read. Measured on Chrome 150.0.7871.46 (fixtures `X-text-plain-doc`, `X-json-doc`,
//! `X-css-doc`, `X-image-doc`), 2026-09-25:
//!
//! ```text
//! text/plain, text/css, text/javascript, text/markdown
//!   <html><head><meta name="color-scheme" content="light dark"></head><body>
//!   <pre style="word-wrap: break-word; white-space: pre-wrap;">TEXT</pre></body></html>
//! application/json
//!   <html><head><meta name="color-scheme" content="light dark"><meta charset="utf-8"></head><body>
//!   <pre>TEXT</pre><div class="json-formatter-container"></div></body></html>
//!   (no style sheet a page can see; the `pre` computes pre-wrap/break-word, the body margin 0)
//! image/png
//!   <html style="height: 100%;"><head><meta name="viewport" content="width=device-width, minimum-scale=0.1">
//!   <title>NAME (W×H)</title></head><body style="margin: 0px; height: 100%; background-color: rgb(14, 14, 14);">
//!   <img style="display: block;-webkit-user-select: none;margin: auto;background-color: hsl(0, 0%, 90%);
//!   transition: background-color 300ms;" src="URL"></body></html>
//! text/csv
//!   a download: no document commits
//! ```
//!
//! korvus put all of them through the HTML parser: markup inside a text file became elements,
//! `innerText` collapsed the lines of an example host's trace into one, and a PNG's bytes became
//! body text.
//!
//! WHICH TYPES is Chromium's rule (`MimeUtil::IsSupportedNonImageMimeType` and the image list), not a
//! guess: every `text/*` except the list Chromium refuses to render (calendars, contacts, CSV and TSV,
//! RTF, ...), the JavaScript and JSON types, and the images Blink decodes.

/// The document a response becomes, when it is not HTML or XML.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Viewer {
    Text,
    Json,
    Image,
}

/// `text/*` types Chromium will not render in a tab (`kUnsupportedTextTypes`): a navigation to one of
/// these is a download.
pub(crate) const UNSUPPORTED_TEXT: &[&str] = &[
    "text/calendar",
    "text/x-calendar",
    "text/x-vcalendar",
    "text/vcalendar",
    "text/vcard",
    "text/x-vcard",
    "text/directory",
    "text/ldif",
    "text/qif",
    "text/x-qif",
    "text/x-csv",
    "text/x-vcf",
    "text/rtf",
    "text/comma-separated-values",
    "text/csv",
    "text/tab-separated-values",
    "text/tsv",
    "text/ofx",
    "text/vnd.sun.j2me.app-descriptor",
    "text/x-ms-iqy",
    "text/x-ms-odc",
    "text/x-ms-rqy",
    "text/x-ms-contact",
];

/// The non-`text/*` script types Chromium renders as text.
const SCRIPT_TYPES: &[&str] = &["application/javascript", "application/ecmascript", "application/x-javascript", "application/x-ecmascript"];

/// The image types Blink decodes into an image document (`kSupportedImageTypes`). `image/svg+xml` is XML, not here.
pub(crate) const IMAGE_TYPES: &[&str] = &[
    "image/png",
    "image/x-png",
    "image/apng",
    "image/jpeg",
    "image/pjpeg",
    "image/jpg",
    "image/gif",
    "image/webp",
    "image/avif",
    "image/bmp",
    "image/x-xbitmap",
    "image/x-icon",
    "image/vnd.microsoft.icon",
];

/// Which viewer a response's MIME type selects, or `None` for HTML, XML and everything else. `mime` is the
/// type the network stack read off every `Content-Type` value (`transport::mime::mime_type_and_charset`, A/B C5):
/// cut here at the first `;` by hand, `text/html,text/html;charset=utf-8` was the "type" `text/html,text/html`
/// and opened in the text viewer.
pub fn viewer_for(mime: &str) -> Option<Viewer> {
    let essence = mime.to_ascii_lowercase();
    if essence.is_empty() || essence == "text/html" || essence == "text/xml" || essence.ends_with("+xml") {
        return None;
    }
    if essence == "application/json" || essence == "text/json" || (essence.starts_with("application/") && essence.ends_with("+json")) {
        return Some(Viewer::Json);
    }
    if IMAGE_TYPES.contains(&essence.as_str()) {
        return Some(Viewer::Image);
    }
    if SCRIPT_TYPES.contains(&essence.as_str()) {
        return Some(Viewer::Text);
    }
    if essence.starts_with("text/") && !UNSUPPORTED_TEXT.contains(&essence.as_str()) {
        return Some(Viewer::Text);
    }
    None
}

/// HTML-escape text for an element's content: the viewer's `<pre>` holds the source TEXT, so `<b>` in a
/// text file is the four characters Chrome shows, never an element.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 16);
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

fn escape_attr(text: &str) -> String {
    escape(text).replace('"', "&quot;")
}

/// The text viewer's document.
pub fn text_document(text: &str) -> String {
    format!(
        "<html><head><meta name=\"color-scheme\" content=\"light dark\"></head><body><pre style=\"word-wrap: break-word; white-space: pre-wrap;\">{}</pre></body></html>",
        escape(text)
    )
}

/// The JSON viewer's document. Its `pre` wraps and its body has no margin by a style the page cannot
/// see (Chrome: `document.styleSheets.length` 0); the caller installs that as a user-agent rule.
pub fn json_document(text: &str) -> String {
    format!(
        "<html><head><meta name=\"color-scheme\" content=\"light dark\"><meta charset=\"utf-8\"></head><body><pre>{}</pre><div class=\"json-formatter-container\"></div></body></html>",
        escape(text)
    )
}

/// The user-agent rules of the JSON viewer's document (not page-visible).
pub const JSON_UA_RULES: &str = "body { margin: 0; } pre { white-space: pre-wrap; word-wrap: break-word; }";

/// The image viewer's document for `url`. `dims` is the image's natural size when the bytes told us:
/// Chrome's title is `NAME (W×H)` once the image is decoded, and the bare name before that.
pub fn image_document(url: &str, dims: Option<(u32, u32)>) -> String {
    let name = url::Url::parse(url)
        .ok()
        .and_then(|u| u.path_segments().and_then(|s| s.last().map(str::to_string)))
        .filter(|s| !s.is_empty())
        .map(|s| percent_decode(&s))
        .unwrap_or_else(|| url.to_string());
    let title = match dims {
        Some((w, h)) => format!("{name} ({w}\u{d7}{h})"),
        None => name,
    };
    format!(
        "<html style=\"height: 100%;\"><head><meta name=\"viewport\" content=\"width=device-width, minimum-scale=0.1\"><title>{}</title></head>\
<body style=\"margin: 0px; height: 100%; background-color: rgb(14, 14, 14);\"><img style=\"display: block;-webkit-user-select: none;margin: auto;background-color: hsl(0, 0%, 90%);transition: background-color 300ms;\" src=\"{}\"></body></html>",
        escape(&title),
        escape_attr(url)
    )
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes.get(i + 1).copied()), hex(bytes.get(i + 2).copied())) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: Option<u8>) -> Option<u8> {
    match b? {
        c @ b'0'..=b'9' => Some(c - b'0'),
        c @ b'a'..=b'f' => Some(c - b'a' + 10),
        c @ b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// The natural size of a PNG, GIF, JPEG or WebP from its first bytes, or `None`.
pub fn image_dims(bytes: &[u8]) -> Option<(u32, u32)> {
    let be32 = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    let le16 = |b: &[u8]| u16::from_le_bytes([b[0], b[1]]) as u32;
    if bytes.len() >= 24 && bytes.starts_with(b"\x89PNG\r\n\x1a\n") && &bytes[12..16] == b"IHDR" {
        return Some((be32(&bytes[16..20]), be32(&bytes[20..24])));
    }
    if bytes.len() >= 10 && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        return Some((le16(&bytes[6..8]), le16(&bytes[8..10])));
    }
    if bytes.len() >= 30 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        match &bytes[12..16] {
            b"VP8X" => {
                let w = 1 + (bytes[24] as u32 | (bytes[25] as u32) << 8 | (bytes[26] as u32) << 16);
                let h = 1 + (bytes[27] as u32 | (bytes[28] as u32) << 8 | (bytes[29] as u32) << 16);
                return Some((w, h));
            }
            b"VP8L" if bytes.len() >= 25 => {
                let b = &bytes[21..25];
                let w = 1 + ((b[0] as u32) | ((b[1] as u32 & 0x3f) << 8));
                let h = 1 + (((b[1] as u32) >> 6) | ((b[2] as u32) << 2) | ((b[3] as u32 & 0x0f) << 10));
                return Some((w, h));
            }
            b"VP8 " if bytes.len() >= 30 => {
                return Some((le16(&bytes[26..28]) & 0x3fff, le16(&bytes[28..30]) & 0x3fff));
            }
            _ => return None,
        }
    }
    if bytes.len() >= 4 && bytes[0] == 0xff && bytes[1] == 0xd8 {
        let mut i = 2;
        while i + 9 < bytes.len() {
            if bytes[i] != 0xff {
                i += 1;
                continue;
            }
            let marker = bytes[i + 1];
            let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
            if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
                let h = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
                let w = u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]) as u32;
                return Some((w, h));
            }
            i += 2 + len;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chromium_decides_which_types_get_a_viewer() {
        // Through the network stack's reading of the field, as `navigation.rs` asks.
        let v = |ct: &str| viewer_for(&transport::mime::mime_type_and_charset_of(ct).mime);
        assert_eq!(v("text/plain; charset=utf-8"), Some(Viewer::Text));
        assert_eq!(v("text/css"), Some(Viewer::Text));
        assert_eq!(v("text/javascript"), Some(Viewer::Text));
        assert_eq!(v("application/javascript"), Some(Viewer::Text));
        assert_eq!(v("text/markdown"), Some(Viewer::Text));
        assert_eq!(v("application/json"), Some(Viewer::Json));
        assert_eq!(v("application/manifest+json"), Some(Viewer::Json));
        assert_eq!(v("image/png"), Some(Viewer::Image));
        assert_eq!(v("text/csv"), None);
        assert_eq!(v("text/html"), None);
        assert_eq!(v("application/xml"), None);
        assert_eq!(v("image/svg+xml"), None);
        assert_eq!(v(""), None);
        // A/B C5: one field, two values -- the last valid one is the type (Chrome 150, measured).
        assert_eq!(v("text/html,text/html;charset=utf-8"), None);
        assert_eq!(v("text/plain,text/html"), None);
        assert_eq!(v("text/html;charset=gbk,text/plain"), Some(Viewer::Text));
    }

    #[test]
    fn the_text_is_text_never_markup() {
        let d = text_document("a <b>x</b> & y\n");
        assert!(d.contains("a &lt;b&gt;x&lt;/b&gt; &amp; y\n</pre>"), "{d}");
    }

    #[test]
    fn the_image_title_carries_its_size() {
        let png = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0, 0, 0, 13, b'I', b'H', b'D', b'R', 0, 0, 0, 1, 0, 0, 0, 1];
        assert_eq!(image_dims(&png), Some((1, 1)));
        assert!(image_document("http://h/i.png", Some((1, 1))).contains("<title>i.png (1\u{d7}1)</title>"));
    }
}
