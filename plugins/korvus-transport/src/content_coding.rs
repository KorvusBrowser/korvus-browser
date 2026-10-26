//! Removing a response's content coding HERE, chunk by chunk, so the bytes that came over the wire can
//! be counted (korvus-analysis U-5).
//!
//! The client used to decode (wreq's tower-http layer), which handed over the decoded body only and
//! stripped `Content-Encoding` / `Content-Length` -- so every `encodedBodySize` and `transferSize` this
//! engine reported was the DECODED size (haveibeenpwned.com's document: 27606 where Chrome 150 reports
//! 7709 zstd bytes and `transferSize` 8009), and `fetch()` / XHR showed no `Content-Encoding` header
//! where Chrome shows the server's. The client's decoding is now off per request (the persona's
//! `Accept-Encoding` header is untouched: the layer only ever ADDED one to a request without it).
//!
//! Codings, as Chrome's `FilterSourceStream` takes them (`GetContentEncodingTypes`, `filter_source_stream.cc:110`):
//! every `Content-Encoding` value, split at commas, ASCII case-insensitively -- `gzip` / `x-gzip`, `deflate`, `br`,
//! `zstd`, undone last-first. Any other token -- `identity` and an empty one included, which Chromium's table maps to
//! "unknown" and "none" -- passes the WHOLE body through as it came, whatever else is listed. A body that does not
//! decode is a network error, Chrome's `ERR_CONTENT_DECODING_FAILED`.
//!
//! THE END OF A BODY IS NEVER AN ERROR. Chromium's streams only fail on bytes they cannot decode: a
//! gzip stream that ends inside its header, its deflate data or its footer, or a brotli or zstd stream cut short,
//! ends with what it decoded (`gzip_source_stream.cc`, `brotli_source_stream.cc`, `zstd_source_stream.cc`: no
//! end-of-input check reaches the reader), and a gzip footer's CRC is skipped, never checked. Here every stage's end
//! was its decoder's `finish`, which demands a complete stream: united.com's `HEAD /en/us` -- `Content-Encoding: gzip`
//! and, as for every HEAD, no body -- failed `net::ERR_CONTENT_DECODING_FAILED` where Chrome answers 200
//! Two more of Chromium's rules ride along: a gzip body is ONE member (bytes
//! after its footer are ignored, `STATE_IGNORING_EXTRA_BYTES`), and a `deflate` body is zlib unless inflating it as
//! zlib fails, in which case it is replayed as a raw stream behind a zlib header of Chromium's own
//! (`InsertZlibHeader`).

use std::io::Write;

/// Chromium's `GZipHeader` (`net/filter/gzip_header.cc`): a gzip member's header, read as it arrives.
#[derive(Default)]
struct GzipHeader {
    /// `IN_HEADER_ID1` (0) ... `IN_DONE` (17), in `gzip_header.h`'s order.
    state: u8,
    flags: u8,
    extra_length: u16,
}

enum HeaderStatus {
    Incomplete,
    /// The header ended this many bytes into the input.
    Complete(usize),
    Invalid,
}

const FLAG_FHCRC: u8 = 0x02;
const FLAG_FEXTRA: u8 = 0x04;
const FLAG_FNAME: u8 = 0x08;
const FLAG_FCOMMENT: u8 = 0x10;
const IN_HEADER_OS: u8 = 9;
const IN_XLEN_BYTE_0: u8 = 10;
const IN_FEXTRA: u8 = 12;
const IN_FNAME: u8 = 13;
const IN_FCOMMENT: u8 = 14;
const IN_FHCRC_BYTE_0: u8 = 15;
const IN_DONE: u8 = 17;

impl GzipHeader {
    /// `GZipHeader::ReadMore`, step for step.
    fn read_more(&mut self, buf: &[u8]) -> HeaderStatus {
        let mut pos = 0;
        while pos < buf.len() {
            match self.state {
                0 | 1 | 2 => {
                    let want = [0x1f, 0x8b, 8][self.state as usize];
                    if buf[pos] != want {
                        return HeaderStatus::Invalid;
                    }
                    pos += 1;
                    self.state += 1;
                }
                3 => {
                    self.flags = buf[pos] & (FLAG_FHCRC | FLAG_FEXTRA | FLAG_FNAME | FLAG_FCOMMENT);
                    pos += 1;
                    self.state += 1;
                }
                // MTIME (4 bytes), XFL, OS
                4..=IN_HEADER_OS => {
                    pos += 1;
                    self.state += 1;
                }
                IN_XLEN_BYTE_0 => {
                    if self.flags & FLAG_FEXTRA == 0 {
                        self.state = IN_FNAME;
                        continue;
                    }
                    self.extra_length = buf[pos] as u16;
                    pos += 1;
                    self.state += 1;
                }
                // IN_XLEN_BYTE_1, falling through into IN_FEXTRA in the same step (so a zero-length FEXTRA ends here)
                11 | IN_FEXTRA => {
                    if self.state == 11 {
                        self.extra_length = self.extra_length.wrapping_add((buf[pos] as u16) << 8);
                        pos += 1;
                        self.state = IN_FEXTRA;
                    }
                    let n = (self.extra_length as usize).min(buf.len() - pos);
                    pos += n;
                    self.extra_length -= n as u16;
                    if self.extra_length == 0 {
                        self.state = IN_FNAME;
                        self.flags &= !FLAG_FEXTRA;
                    }
                }
                IN_FNAME | IN_FCOMMENT => {
                    let flag = if self.state == IN_FNAME { FLAG_FNAME } else { FLAG_FCOMMENT };
                    if self.flags & flag == 0 {
                        self.state += 1;
                        continue;
                    }
                    // the field ends at its NUL
                    match buf[pos..].iter().position(|&b| b == 0) {
                        Some(i) => {
                            pos += i + 1;
                            self.flags &= !flag;
                            self.state += 1;
                        }
                        None => pos = buf.len(),
                    }
                }
                IN_FHCRC_BYTE_0 => {
                    if self.flags & FLAG_FHCRC == 0 {
                        self.state = IN_DONE;
                        continue;
                    }
                    pos += 1;
                    self.state += 1;
                }
                16 => {
                    pos += 1;
                    self.flags &= !FLAG_FHCRC;
                    self.state += 1;
                }
                _ => return HeaderStatus::Complete(pos),
            }
        }
        if self.state > IN_HEADER_OS && self.flags == 0 {
            HeaderStatus::Complete(pos)
        } else {
            HeaderStatus::Incomplete
        }
    }
}

/// Where a `gzip` / `deflate` stream is (`GzipSourceStream::InputState`).
enum InflateState {
    GzipHeader,
    SniffingDeflateHeader,
    CompressedBody,
    GzipFooter,
    IgnoringExtraBytes,
}

/// Chromium's `GzipSourceStream` (`net/filter/gzip_source_stream.cc`), for `gzip` and `deflate`.
struct Inflate {
    state: InflateState,
    header: GzipHeader,
    z: flate2::Decompress,
    /// `deflate` only: what was fed while sniffing for a zlib header, replayed if there was none.
    replay: Vec<u8>,
    footer_left: usize,
}

/// `kMaxZlibHeaderSniffBytes`: how much of a `deflate` body may go by, nothing decoded yet, before it is taken for zlib.
const MAX_ZLIB_HEADER_SNIFF_BYTES: usize = 1000;

/// One `inflate(Z_NO_FLUSH)` over all of `input`, the output appended to `out`: `(bytes of input consumed, stream
/// end reached)`. zlib's own errors (`Z_DATA_ERROR`, `Z_NEED_DICT`, ...) are the error.
fn inflate_into(z: &mut flate2::Decompress, input: &[u8], out: &mut Vec<u8>) -> std::io::Result<(usize, bool)> {
    let start = z.total_in();
    loop {
        if out.capacity() - out.len() < 16 * 1024 {
            out.reserve(32 * 1024);
        }
        let (in0, out0) = (z.total_in(), z.total_out());
        let used_so_far = (in0 - start) as usize;
        let status = z.decompress_vec(&input[used_so_far..], out, flate2::FlushDecompress::None).map_err(std::io::Error::other)?;
        let consumed = (z.total_in() - start) as usize;
        if matches!(status, flate2::Status::StreamEnd) {
            return Ok((consumed, true));
        }
        let progressed = z.total_in() != in0 || z.total_out() != out0;
        // All input taken and room left for more output: zlib has said everything it can for now.
        if !progressed || (consumed == input.len() && out.len() < out.capacity()) {
            return Ok((consumed, false));
        }
    }
}

impl Inflate {
    fn gzip() -> Inflate {
        Inflate { state: InflateState::GzipHeader, header: GzipHeader::default(), z: flate2::Decompress::new(false), replay: Vec::new(), footer_left: 0 }
    }

    fn deflate() -> Inflate {
        Inflate { state: InflateState::SniffingDeflateHeader, header: GzipHeader::default(), z: flate2::Decompress::new(true), replay: Vec::new(), footer_left: 0 }
    }

    /// `FilterData`: everything `input` decodes to.
    fn feed(&mut self, input: &[u8]) -> std::io::Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut data = input;
        while !data.is_empty() {
            match self.state {
                InflateState::GzipHeader => match self.header.read_more(data) {
                    HeaderStatus::Incomplete => data = &[],
                    HeaderStatus::Complete(end) => {
                        // a valid header promises a footer: CRC32 and ISIZE, skipped
                        self.footer_left = 8;
                        data = &data[end..];
                        self.state = InflateState::CompressedBody;
                    }
                    HeaderStatus::Invalid => return Err(std::io::Error::other("invalid gzip header")),
                },
                InflateState::SniffingDeflateHeader => {
                    let mut sniffed = Vec::new();
                    match inflate_into(&mut self.z, data, &mut sniffed) {
                        // Not zlib: a raw stream, then. A zlib header of its own goes in front, and what went by while
                        // sniffing is replayed through it; `data` itself was not consumed, so it follows as it is.
                        Err(_) => {
                            let mut z = flate2::Decompress::new(true);
                            let mut sink = Vec::with_capacity(4);
                            z.decompress_vec(&[0x78, 0x01], &mut sink, flate2::FlushDecompress::None).map_err(std::io::Error::other)?;
                            self.z = z;
                            self.state = InflateState::CompressedBody;
                            let replay = std::mem::take(&mut self.replay);
                            if !replay.is_empty() {
                                out.extend(self.feed(&replay)?);
                            }
                        }
                        Ok((used, end)) => {
                            if !sniffed.is_empty() || used + self.replay.len() >= MAX_ZLIB_HEADER_SNIFF_BYTES || end {
                                self.replay.clear();
                                self.state = if end { InflateState::GzipFooter } else { InflateState::CompressedBody };
                                out.extend(sniffed);
                            } else {
                                self.replay.extend_from_slice(&data[..used]);
                            }
                            data = &data[used..];
                            if used == 0 && !end {
                                break;
                            }
                        }
                    }
                }
                InflateState::CompressedBody => {
                    let (used, end) = inflate_into(&mut self.z, data, &mut out)?;
                    data = &data[used..];
                    if end {
                        self.state = InflateState::GzipFooter;
                    } else if used == 0 {
                        break;
                    }
                }
                InflateState::GzipFooter => {
                    let n = self.footer_left.min(data.len());
                    self.footer_left -= n;
                    data = &data[n..];
                    if self.footer_left == 0 {
                        self.state = InflateState::IgnoringExtraBytes;
                    }
                }
                InflateState::IgnoringExtraBytes => data = &[],
            }
        }
        Ok(out)
    }
}

enum Stage {
    /// `gzip` and `deflate` (see [`Inflate`]).
    Inflate(Box<Inflate>),
    Brotli(Box<brotli::DecompressorWriter<Vec<u8>>>),
    Zstd(zstd::stream::write::Decoder<'static, Vec<u8>>),
}

impl Stage {
    /// The stage for one coding token (lowercase); `None` for any token Chromium does not decode, which leaves the
    /// whole body as it came.
    fn of(coding: &str) -> Option<Stage> {
        Some(match coding {
            "gzip" | "x-gzip" => Stage::Inflate(Box::new(Inflate::gzip())),
            "deflate" => Stage::Inflate(Box::new(Inflate::deflate())),
            "br" => Stage::Brotli(Box::new(brotli::DecompressorWriter::new(Vec::new(), 4096))),
            "zstd" => Stage::Zstd(zstd::stream::write::Decoder::new(Vec::new()).ok()?),
            _ => return None,
        })
    }

    /// Feed `input`; answer what came out so far.
    fn feed(&mut self, input: &[u8]) -> std::io::Result<Vec<u8>> {
        match self {
            Stage::Inflate(d) => d.feed(input),
            Stage::Brotli(d) => {
                d.write_all(input)?;
                Ok(std::mem::take(d.get_mut()))
            }
            Stage::Zstd(d) => {
                d.write_all(input)?;
                d.flush()?;
                Ok(std::mem::take(d.get_mut()))
            }
        }
    }

    /// The end of the input: what was still held back. Never an error for a stream cut short (see the module docs).
    fn finish(self) -> std::io::Result<Vec<u8>> {
        match self {
            // everything decodable was handed out as it was fed
            Stage::Inflate(_) => Ok(Vec::new()),
            // `Err` is "the stream did not end": what it decoded is the body
            Stage::Brotli(d) => Ok(match d.into_inner() {
                Ok(w) | Err(w) => w,
            }),
            Stage::Zstd(mut d) => {
                d.flush()?;
                Ok(d.into_inner())
            }
        }
    }
}

/// One response body's decoder, and the count of the bytes it was fed (the ENCODED size).
pub(crate) struct BodyDecoder {
    /// Innermost coding last: the body goes through `stages[0]` first.
    stages: Vec<Stage>,
    encoded: u64,
}

impl BodyDecoder {
    /// The decoder for a response with these headers.
    pub(crate) fn for_headers(headers: &[(String, String)]) -> BodyDecoder {
        // every value, split at commas -- an EMPTY one included (`HttpUtil::ValuesIterator`, `ignore_empty_values`
        // false) -- its surrounding whitespace trimmed
        let codings: Vec<String> = headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("content-encoding"))
            .flat_map(|(_, v)| v.split(','))
            .map(|c| c.trim_matches([' ', '\t']).to_ascii_lowercase())
            .collect();
        let mut stages = Vec::new();
        for c in codings.iter().rev() {
            match Stage::of(c) {
                Some(s) => stages.push(s),
                // `identity`, an empty token or one Chromium does not know: nothing is undone at all
                None => return BodyDecoder { stages: Vec::new(), encoded: 0 },
            }
        }
        BodyDecoder { stages, encoded: 0 }
    }

    /// Feed one chunk off the wire; answer its decoded bytes (possibly none yet).
    pub(crate) fn feed(&mut self, chunk: &[u8]) -> anyhow::Result<Vec<u8>> {
        self.encoded += chunk.len() as u64;
        let mut data = chunk.to_vec();
        for s in &mut self.stages {
            data = s.feed(&data).map_err(|e| anyhow::anyhow!("net::ERR_CONTENT_DECODING_FAILED: {e}"))?;
        }
        Ok(data)
    }

    /// The end of the body: the decoded bytes still held, and the encoded size. Each stage ends in
    /// turn, what it still held going through the stages after it first.
    pub(crate) fn finish(self) -> anyhow::Result<(Vec<u8>, u64)> {
        let fail = |e: std::io::Error| anyhow::anyhow!("net::ERR_CONTENT_DECODING_FAILED: {e}");
        let mut carry: Vec<u8> = Vec::new();
        for mut s in self.stages {
            let mut out = if carry.is_empty() { Vec::new() } else { s.feed(&carry).map_err(fail)? };
            out.extend(s.finish().map_err(fail)?);
            carry = out;
        }
        Ok((carry, self.encoded))
    }

    /// Whether the body is being decoded at all.
    #[cfg(test)]
    fn decodes(&self) -> bool {
        !self.stages.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdr(v: &str) -> Vec<(String, String)> {
        vec![("Content-Encoding".to_string(), v.to_string())]
    }

    fn roundtrip(headers: &[(String, String)], encoded: &[u8], chunk: usize) -> (Vec<u8>, u64) {
        let mut d = BodyDecoder::for_headers(headers);
        let mut out = Vec::new();
        for c in encoded.chunks(chunk.max(1)) {
            out.extend(d.feed(c).unwrap());
        }
        let (tail, n) = d.finish().unwrap();
        out.extend(tail);
        (out, n)
    }

    const TEXT: &[u8] = b"<!doctype html><title>t</title><p>korvus content coding round trip</p>";

    #[test]
    fn every_coding_decodes_and_counts_the_wire_bytes() {
        let gz = {
            let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(TEXT).unwrap();
            e.finish().unwrap()
        };
        let zl = {
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(TEXT).unwrap();
            e.finish().unwrap()
        };
        let raw = {
            let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(TEXT).unwrap();
            e.finish().unwrap()
        };
        let br = {
            let mut w = brotli::CompressorWriter::new(Vec::new(), 4096, 5, 22);
            w.write_all(TEXT).unwrap();
            w.into_inner()
        };
        let zs = zstd::stream::encode_all(TEXT, 3).unwrap();
        for (coding, bytes) in [("gzip", &gz), ("x-gzip", &gz), ("deflate", &zl), ("deflate", &raw), ("br", &br), ("zstd", &zs)] {
            for chunk in [1, 3, 4096] {
                let (out, n) = roundtrip(&hdr(coding), bytes, chunk);
                assert_eq!(out, TEXT, "{coding} in {chunk}-byte chunks");
                assert_eq!(n, bytes.len() as u64, "{coding}: the encoded count is the wire count");
            }
        }
        // two codings, undone last-first
        let gz_br = {
            let mut w = brotli::CompressorWriter::new(Vec::new(), 4096, 5, 22);
            w.write_all(&gz).unwrap();
            w.into_inner()
        };
        assert_eq!(roundtrip(&hdr("gzip, br"), &gz_br, 7).0, TEXT);
    }

    #[test]
    fn identity_and_unknown_codings_pass_the_body_through() {
        for h in [Vec::new(), hdr("identity"), hdr("compress"), hdr("gzip, compress")] {
            assert!(!BodyDecoder::for_headers(&h).decodes() || h.is_empty());
            assert_eq!(roundtrip(&h, TEXT, 5), (TEXT.to_vec(), TEXT.len() as u64));
        }
    }

    #[test]
    fn a_body_that_does_not_decode_is_a_network_error() {
        let mut d = BodyDecoder::for_headers(&hdr("gzip"));
        let failed = d.feed(b"this is not gzip at all").is_err() || d.finish().is_err();
        assert!(failed);
    }

    fn gz(data: &[u8], level: u32) -> Vec<u8> {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(level));
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    // Chromium's ends (the page-level twin is korvus-runtime's `content_coding_live`): an empty body and
    // one cut short end with what decoded -- never `ERR_CONTENT_DECODING_FAILED` -- for every coding.
    #[test]
    fn an_empty_or_truncated_body_ends_with_what_it_decoded() {
        for coding in ["gzip", "deflate", "br", "zstd", "gzip, br"] {
            assert_eq!(roundtrip(&hdr(coding), b"", 1), (Vec::new(), 0), "an empty `{coding}` body (a HEAD's)");
        }
        let text: Vec<u8> = (0..1240u32).map(|i| b'a' + (i % 26) as u8).collect();
        // a STORED stream cut inside its block: every byte of it that arrived comes out (the exact count is graded on
        // Python-made bytes by `content_coding_live`)
        let stored = gz(&text, 0);
        let out = roundtrip(&hdr("gzip"), &stored[..600], 7).0;
        assert!(out.len() >= 500 && text.starts_with(&out), "{} bytes", out.len());
        assert_eq!(roundtrip(&hdr("gzip"), &stored[..5], 1).0, Vec::<u8>::new(), "inside the header");
        let br = {
            let mut w = brotli::CompressorWriter::new(Vec::new(), 4096, 5, 22);
            w.write_all(&text).unwrap();
            w.into_inner()
        };
        let mut d = BodyDecoder::for_headers(&hdr("br"));
        let mut out = d.feed(&br[..br.len() / 2]).unwrap();
        out.extend(d.finish().expect("a brotli stream cut short is not an error").0);
        assert!(text.starts_with(&out), "what decoded is the start of the body");
    }

    // A gzip footer is skipped, not checked; a body is ONE member; `identity` or an empty token anywhere leaves the
    // whole body as it came (Chrome 150, `content_coding_live`'s `gz-badcrc`, `gz-two`, `gz-identity`, `gz-empty-token`).
    #[test]
    fn gzip_is_one_member_its_crc_unchecked_and_identity_passes_it_through() {
        let a = gz(TEXT, 6);
        let mut bad = a.clone();
        let n = bad.len();
        bad[n - 8] ^= 0xff;
        assert_eq!(roundtrip(&hdr("gzip"), &bad, 4).0, TEXT);
        let two = [a.clone(), gz(b"the second member", 6)].concat();
        assert_eq!(roundtrip(&hdr("gzip"), &two, 3).0, TEXT);
        let junk = [a.clone(), b"trailing junk".to_vec()].concat();
        assert_eq!(roundtrip(&hdr("gzip"), &junk, 4096).0, TEXT);
        for h in [hdr("gzip, identity"), hdr("gzip,"), vec![("content-encoding".into(), "identity".into()), ("content-encoding".into(), "gzip".into())]] {
            assert_eq!(roundtrip(&h, &a, 9).0, a, "{h:?}");
        }
        assert_eq!(roundtrip(&hdr("X-GZIP"), &a, 9).0, TEXT, "a token is matched case-insensitively");
    }
}
