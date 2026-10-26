//! `TextEncoder` and `TextDecoder` (encoding.spec.whatwg.org), OWN classes.
//!
//! They were `korvus-runtime`'s `#[op2]` classes `JsTextEncoder` / `JsTextDecoder` -- Web API in the
//! kernel, which invariant I9 forbids -- published on the global by the bootstrap and renamed there.
//! Moved here verbatim, bodies and all, as `#[webapi(own)]` classes: each interface is its table below
//! (`class JsTextEncoder` / `class JsTextDecoder`), built by `build_newable` in both realms' class slot
//! (`install::live::bootstrap`). The argument conversions are op2's, unchanged -- including its WebIDL
//! prefix (`'call' on 'JsTextEncoder'`) and its buffer sentences, each a B-row candidate of its own.

use macros::webapi;

/// Native `TextEncoder` (UTF-8).
#[repr(C)]
pub struct JsTextEncoder;

/// `encodeInto`'s result — the WebIDL `TextEncoderEncodeIntoResult` dictionary: `read` UTF-16 code
/// units of the source consumed, `written` bytes stored into the destination.
#[derive(serde::Serialize)]
struct TextEncoderEncodeIntoResult {
    read: usize,
    written: usize,
}

/// Arity guard for `encodeInto` — Chrome's exact WebIDL TypeError. (deno_ops' generated `#[required]`
/// message would name the internal `call` fn on `JsTextEncoder`; the JS-visible names are
/// `encodeInto` on `TextEncoder`. `#[validate]` runs before argument conversion, so a missing
/// destination reports arity — like Chrome — rather than a buffer-conversion error.)
fn te_encode_into_arity<'s>(
    _scope: &mut deno_core::v8::PinScope<'s, '_>,
    args: &deno_core::v8::FunctionCallbackArguments,
) -> Result<(), deno_error::JsErrorBox> {
    let present = args.length();
    if present < 2 {
        return Err(deno_error::JsErrorBox::type_error(format!(
            "Failed to execute 'encodeInto' on 'TextEncoder': 2 arguments required, but only {present} present."
        )));
    }
    Ok(())
}

#[webapi(own, name = "TextEncoder")]
impl JsTextEncoder {
    #[constructor]
    #[cppgc]
    fn new() -> JsTextEncoder {
        JsTextEncoder
    }
    #[getter]
    #[string]
    fn encoding(&self) -> String {
        "utf-8".to_string()
    }
    // WebIDL `encode(optional USVString input = "")`. `#[webidl(default = "")]` reproduces Chrome's
    // exact argument conversion: an ABSENT argument binds the "" default (WPT
    // encoding/textencoder-utf16-surrogates "USVString default" — never the stringification of
    // `undefined`), any other value goes through WebIDL ToString coercion (`encode(null)` encodes
    // "null", `encode(123)` encodes "123"), a Symbol throws TypeError, and lone surrogates are
    // replaced with U+FFFD per USVString rules (WPT textencoder-utf16-surrogates) — the plain
    // `#[string] Option<String>` path bound null to None and stringified non-strings to "", both
    // Chrome divergences.
    #[buffer]
    fn encode(&self, #[webidl(default = "")] input: String) -> Vec<u8> {
        input.into_bytes()
    }
    // WebIDL `TextEncoderEncodeIntoResult encodeInto(USVString source, [AllowShared] Uint8Array
    // destination)`: UTF-8-encode as much of `source` as the destination holds, IN PLACE, and report
    // progress so a caller can loop. Per the Encoding standard a code point is stored whole or not at
    // all — when the remaining bytes cannot hold the next code point, conversion stops and that code
    // point's code units do NOT count toward `read` (a split surrogate pair is simply re-fed next
    // round). `#[buffer] &mut [u8]` binds exactly a Uint8Array (the declared arg type) as a live view,
    // so the stored bytes are immediately visible to JS. Arity is guarded by the validator above
    // (Chrome's message); two non-optional args give the native fn length 2, matching Brave.
    // `#[webidl]` gives the source Chrome's USVString conversion (ToString coercion for null/numbers,
    // TypeError for Symbols, lone surrogates -> U+FFFD) before the loop below counts code units.
    #[validate(te_encode_into_arity)]
    #[serde]
    fn encode_into(&self, #[webidl] source: String, #[buffer] destination: &mut [u8]) -> TextEncoderEncodeIntoResult {
        let (mut read, mut written) = (0usize, 0usize);
        for ch in source.chars() {
            let len = ch.len_utf8();
            if written + len > destination.len() {
                break; // not enough bytes left for this code point — stop without consuming it
            }
            let mut b = [0u8; 4];
            destination[written..written + len].copy_from_slice(ch.encode_utf8(&mut b).as_bytes());
            written += len;
            read += ch.len_utf16();
        }
        TextEncoderEncodeIntoResult { read, written }
    }
}

/// Native `TextDecoder` — honors the encoding label (UTF-8, Shift_JIS, EUC-JP, ISO-8859-*, …) via
/// `encoding_rs`, matching the WHATWG Encoding standard. An unknown label throws `RangeError` (Chrome
/// parity — a lenient stub that silently fell back to UTF-8 would be a fingerprint tell).
/// `TextDecoder` construct options — WHATWG `TextDecoderOptions` (`fatal`, `ignoreBOM`). Absent -> both
/// false. `ignoreBOM` keeps its spec spelling (not `ignoreBom`) so `{ ignoreBOM: true }` binds.
#[derive(serde::Deserialize, Default)]
struct TextDecoderOptions {
    #[serde(default)]
    fatal: bool,
    #[serde(default, rename = "ignoreBOM")]
    ignore_bom: bool,
}

/// `decode(input, { stream })` options. `stream: true` keeps an incomplete trailing byte sequence
/// buffered on the decoder for the next call; the default (false) flushes it (emitting U+FFFD, or a
/// TypeError in fatal mode).
#[derive(serde::Deserialize, Default)]
struct TextDecodeOptions {
    #[serde(default)]
    stream: bool,
}

/// A fresh streaming decoder for this encoding, honoring `ignoreBOM` (a decoder that strips a leading
/// BOM vs one that passes it through). Made anew after every non-streaming `decode()` so independent
/// decodes never leak state into one another.
fn fresh_decoder(encoding: &'static encoding_rs::Encoding, ignore_bom: bool) -> encoding_rs::Decoder {
    if ignore_bom { encoding.new_decoder_without_bom_handling() } else { encoding.new_decoder_with_bom_removal() }
}

#[repr(C)]
pub struct JsTextDecoder {
    encoding: &'static encoding_rs::Encoding,
    fatal: bool,
    ignore_bom: bool,
    // Streaming state: retains a partial multi-byte sequence between `decode(.., {stream:true})` calls.
    decoder: std::cell::RefCell<encoding_rs::Decoder>,
}
#[webapi(own, name = "TextDecoder")]
impl JsTextDecoder {
    // `reentrant`: TextDecoderOptions is read property-by-property, so an options object with native
    // accessors re-enters an op. See the note on op_fetch.
    #[constructor]
    #[cppgc]
    #[reentrant]
    fn new(#[string] label: Option<String>, #[serde] #[dict("TextDecoderOptions")] options: Option<TextDecoderOptions>) -> Result<JsTextDecoder, deno_error::JsErrorBox> {
        let encoding = match label.as_deref().map(str::trim) {
            None => encoding_rs::UTF_8,
            Some(l) => encoding_rs::Encoding::for_label(l.as_bytes()).ok_or_else(|| {
                deno_error::JsErrorBox::range_error(format!("The encoding label provided ('{l}') is invalid."))
            })?,
        };
        let o = options.unwrap_or_default();
        let decoder = std::cell::RefCell::new(fresh_decoder(encoding, o.ignore_bom));
        Ok(JsTextDecoder { encoding, fatal: o.fatal, ignore_bom: o.ignore_bom, decoder })
    }
    #[getter]
    #[string]
    fn encoding(&self) -> String {
        // WHATWG: the encoding's name, ASCII-lowercased (e.g. "utf-8", "shift_jis").
        self.encoding.name().to_ascii_lowercase()
    }
    #[getter]
    fn fatal(&self) -> bool {
        self.fatal
    }
    // op2 camelCases the getter name (ignore_bom -> ignoreBom); `rename` forces the spec spelling
    // `ignoreBOM` verbatim (it bypasses the camelCase pass), so the member matches Brave exactly and
    // does NOT read as a korvus-only TELL.
    #[getter]
    #[rename("ignoreBOM")]
    fn ignore_bom(&self) -> bool {
        self.ignore_bom
    }
    // WebIDL: decode(BufferSource? input) -- BufferSource is ArrayBuffer OR any ArrayBufferView, not
    // just a typed-array view. `#[buffer]` (deno_core op2) only binds a TypedArray/DataView; a bare
    // `new TextDecoder().decode(someArrayBuffer)` (no view) would throw "expected typed ArrayBufferView"
    // instead of decoding. `#[anybuffer]` accepts BOTH forms (BufferSource::Any -> to_v8_slice_any).
    // `Option` so `decode()` / `decode(undefined)` mean an empty input ("") rather than a type error.
    // WebIDL: decode(BufferSource? input, optional TextDecodeOptions). `#[anybuffer]` accepts an
    // ArrayBuffer or any view; `Option` makes `decode()` / `decode(undefined)` an empty input. The
    // options carry `stream`: with it the decoder retains a trailing partial sequence for the next
    // call, and a subsequent non-stream decode flushes it (WPT textdecoder-arguments/-eof).
    #[string]
    fn decode(&self, #[anybuffer] bytes: Option<&[u8]>, #[serde] #[dict("TextDecodeOptions")] options: Option<TextDecodeOptions>) -> Result<String, deno_error::JsErrorBox> {
        let bytes = bytes.unwrap_or(&[]);
        let last = !options.unwrap_or_default().stream;
        let mut dec = self.decoder.borrow_mut();
        let cap = dec.max_utf8_buffer_length(bytes.len()).unwrap_or(bytes.len().saturating_mul(4) + 4);
        let mut out = String::with_capacity(cap);
        if self.fatal {
            // fatal flag set: no U+FFFD substitution -- a malformed sequence is a TypeError.
            let (res, _read) = dec.decode_to_string_without_replacement(bytes, &mut out, last);
            if let encoding_rs::DecoderResult::Malformed(_, _) = res {
                *dec = fresh_decoder(self.encoding, self.ignore_bom); // a fatal error ends the stream
                return Err(deno_error::JsErrorBox::type_error("The encoded data was not valid."));
            }
        } else {
            // Replacement on error (U+FFFD); a matching leading BOM is stripped unless ignoreBOM.
            let (_res, _read, _had_errors) = dec.decode_to_string(bytes, &mut out, last);
        }
        if last {
            // A non-streaming decode flushed and finished; reset so the next decode is independent.
            *dec = fresh_decoder(self.encoding, self.ignore_bom);
        }
        Ok(out)
    }
}

webapi::interface! {
    "TextEncoder" [value, both, class JsTextEncoder] {
        ctor   "TextEncoder"                          len 0 = stub,
        attr   "encoding"   : DomString readonly            = cppgc,
        method "encode"     : Any       len 0               = cppgc,
        method "encodeInto" : Any       len 2 required 2    = cppgc,
    }
}

webapi::interface! {
    "TextDecoder" [value, both, class JsTextDecoder] {
        ctor   "TextDecoder"                          len 0 = stub,
        attr   "encoding"   : DomString readonly            = cppgc,
        attr   "fatal"      : Boolean   readonly            = cppgc,
        attr   "ignoreBOM"  : Boolean   readonly            = cppgc,
        method "decode"     : UsvString len 0               = cppgc,
    }
}
