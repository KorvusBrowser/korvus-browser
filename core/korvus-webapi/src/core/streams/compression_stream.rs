//! `core/streams/compression_stream.rs` — `CompressionStream` / `DecompressionStream`.
//!
//! Both were registry stubs: the constructor made an object whose `readable` and `writable` read
//! `undefined`, so `rs.pipeThrough(new DecompressionStream('gzip'))` threw Chrome's own sentence
//! `Failed to execute 'pipeThrough' on 'ReadableStream': Failed to read the 'writable' property from
//! 'ReadableWritablePair': Failed to convert value to 'WritableStream'.` — the largest engine-named
//! cluster in the live corpus's throwing rows.
//!
//! Each is a transform in the WHATWG Compression sense, built the way `TextDecoderStream` is
//! and `TransformStream` before it: a REAL `WritableStream` whose native sink runs the codec and
//! enqueues into a REAL `ReadableStream`. There is no second stream mechanism here.
//!
//! MEASURED on `google-chrome` 150.0.7871.46, against a loopback page, recorded twice, identical:
//!
//! ```text
//! CompressionStream.length / name                   1 / CompressionStream
//! proto own names                                   readable,writable,constructor   (+ Symbol.toStringTag)
//! proto readable/writable descriptor                get:fn,set:undefined,enumerable:true,configurable:true
//! new CompressionStream('gzip') instanceof TransformStream   false   (proto's proto is Object.prototype)
//! cs.readable / cs.writable                         ReadableStream / WritableStream
//! a foreign receiver on the readable getter         TypeError: Illegal invocation
//! CompressionStream('gzip')                         TypeError: Failed to construct 'CompressionStream': Please use the 'new' operator, this DOM object constructor cannot be called as a function.
//! new CompressionStream()                           TypeError: Failed to construct 'CompressionStream': 1 argument required, but only 0 present.
//! accepted formats                                  gzip, deflate, deflate-raw -- and NOTHING else:
//!   'br' 'brotli' 'zstd' 'GZIP' 'gzip ' '' 'dcb' 'dcz' 'identity' 'x-gzip' 'compress'
//!                                                   TypeError: Failed to construct 'CompressionStream': Unsupported compression format: '<s>'
//!   undefined / null / 1                            ... format: 'undefined' / 'null' / '1'   (the argument is stringified first)
//!   {toString(){throw Error('boom')}}               Error: boom
//!   Symbol()                                        TypeError: Failed to construct 'CompressionStream': Cannot convert a Symbol value to a string
//! compress "hello hello hello hello world"
//!   gzip        chunks 10,23   1f8b0800000000000003 cb48cdc9c957c8c020cbf38b725200 336534341d000000
//!   deflate     chunks  2,19   789c                 cb48cdc9c957c8c020cbf38b725200 a38a0af9
//!   deflate-raw chunks    15                        cb48cdc9c957c8c020cbf38b725200
//! compress nothing at all      gzip 20 / deflate 8 / deflate-raw 2, in ONE chunk
//! decompress 1 MB of zeros     out chunks 65536,65536,... (a 65536-byte output buffer)
//! gzip header                  1f8b 08 00 00000000 00 03  -- MTIME always 0, XFL 0, OS 3; stable across a second
//! decompress reads MTIME/XFL/OS/FNAME as data       "gzip with FNAME" / "os byte 0xff" / "mtime nonzero" all decode
//! a non-BufferSource chunk     write AND read reject TypeError: The provided value is not of type '(ArrayBuffer or ArrayBufferView)'.
//! a SharedArrayBuffer chunk                         ... TypeError: Failed to convert value to 'ArrayBuffer'.
//! a view onto shared memory                         ... TypeError: The provided ArrayBufferView value must not be shared.
//! decompress [1,2,3,4,5] as gzip / raw deflate as 'deflate' / zlib as 'gzip'
//!                              TypeError: The compressed data was not valid: incorrect header check.
//! decompress a zlib header whose FCHECK passes but CM != 8
//!                              TypeError: The compressed data was not valid: unknown compression method.
//! decompress with a flipped CRC byte
//!                              TypeError: The compressed data was not valid: incorrect data check.
//! decompress with a flipped ISIZE byte
//!                              TypeError: The compressed data was not valid: incorrect length check.
//! decompress a truncated member / a bare header / nothing at all
//!                              TypeError: Compressed input was truncated.
//! ANY byte after the end -- [0], [0,1,2], [0x1f,0x8b], a whole second gzip member, split or not
//!                              TypeError: Junk found after end of compressed data.
//! ```
//!
//! THE ONE PLACE THIS ENGINE CANNOT ANSWER CHROME'S SENTENCE. Chrome's data errors carry zlib's own
//! `msg`; this build's deflate is `flate2`'s pure-Rust backend, whose failure carries no message at
//! all (`ffi/miniz_oxide.rs`: `ErrorMessage::get -> None`). The conditions THIS file decides itself --
//! every header field, both checksums, the length check, truncation and trailing bytes -- answer
//! Chrome's sentence exactly, and so do the two a malformed first block decides from RFC 1951 alone
//! (`invalid block type`, `invalid stored block lengths`). A corruption deeper in the bitstream errors
//! the stream with the same `TypeError` and the same sentence MINUS the detail, rather than carrying an
//! invented one: `The compressed data was not valid.`
//!
//! AND THE ONE PLACE THE BYTES DIFFER, measured rather than assumed: `flate2`'s `miniz_oxide` encoder
//! agrees with zlib 1.3 byte for byte on short inputs (every reading in the table above) and diverges
//! on larger ones -- 100 KB of structured text compresses to 1155 bytes here against zlib's 1158. The
//! output is valid `deflate` either way and round-trips through both engines; it is not the same
//! encoding of it. Closing that needs a zlib-compatible encoder, not a change here.

use deno_core::{v8, CppgcBase};
use macros::webapi;

use crate::binding::protos::{build_fn_data, get_prop, set_prop, throw_type_error};
use crate::install::live::{getter_row, set_table_proto};
use crate::install::MemberDef;
use super::readable_stream::{rs_close, rs_enqueue, rs_error};

// ── the three formats ───────────────────────────────────────────────────────────────────────────

/// The `CompressionFormat` enumeration, as Chrome 150 accepts it: an EXACT match on one of three
/// ASCII spellings. No trimming, no case folding, no `br`/`brotli`/`zstd` -- all measured above.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Format {
    Deflate,
    DeflateRaw,
    Gzip,
}

impl Format {
    fn parse(s: &str) -> Option<Format> {
        match s {
            "deflate" => Some(Format::Deflate),
            "deflate-raw" => Some(Format::DeflateRaw),
            "gzip" => Some(Format::Gzip),
            _ => None,
        }
    }

    /// The bytes that precede the deflate stream. Chrome's gzip member always carries MTIME 0, XFL 0
    /// and OS 3, and its zlib header is always `78 9c` (CINFO 7, CM 8, level 6, no dictionary).
    fn header(self) -> &'static [u8] {
        match self {
            Format::Gzip => &[0x1f, 0x8b, 0x08, 0x00, 0, 0, 0, 0, 0x00, 0x03],
            Format::Deflate => &[0x78, 0x9c],
            Format::DeflateRaw => &[],
        }
    }

    /// How many bytes follow the deflate stream: gzip's CRC32 + ISIZE, zlib's ADLER32, raw's nothing.
    fn trailer_len(self) -> usize {
        match self {
            Format::Gzip => 8,
            Format::Deflate => 4,
            Format::DeflateRaw => 0,
        }
    }
}

/// Chrome's deflate output buffer: a full one is enqueued and the loop goes round again (measured --
/// 1 MB in gave `10,16384,8223`).
const DEFLATE_BUF: usize = 16384;
/// Chrome's inflate output buffer (measured -- 1 MB of zeros came back as sixteen 65536-byte chunks).
const INFLATE_BUF: usize = 65536;

/// RFC 1950 ADLER-32, over a running value seeded at 1.
fn adler32(seed: u32, buf: &[u8]) -> u32 {
    let (mut s1, mut s2) = (seed & 0xffff, (seed >> 16) & 0xffff);
    // 5552 is the largest n for which 255n(n+1)/2 + (n+1)(65520) fits in 32 bits.
    for block in buf.chunks(5552) {
        for &b in block {
            s1 += b as u32;
            s2 += s1;
        }
        s1 %= 65521;
        s2 %= 65521;
    }
    (s2 << 16) | s1
}

// ── the chunk argument ──────────────────────────────────────────────────────────────────────────

/// A written chunk's bytes, or the sentence Chrome errors the stream with. Copied EAGERLY (the WPT
/// detach tests transfer the caller's buffer out from under a pending write, and Chrome's output is
/// unaffected because the bytes were taken at write time).
fn chunk_bytes(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Result<Vec<u8>, &'static str> {
    if let Ok(view) = v8::Local::<v8::ArrayBufferView>::try_from(v) {
        if let Some(ab) = view.buffer(scope) {
            if ab.get_backing_store().is_shared() {
                return Err("The provided ArrayBufferView value must not be shared.");
            }
        }
        let mut buf = vec![0u8; view.byte_length()];
        view.copy_contents(&mut buf);
        return Ok(buf);
    }
    if v.is_shared_array_buffer() {
        return Err("Failed to convert value to 'ArrayBuffer'.");
    }
    if let Ok(ab) = v8::Local::<v8::ArrayBuffer>::try_from(v) {
        let store = ab.get_backing_store();
        let n = store.byte_length();
        let mut out = vec![0u8; n];
        if n > 0 {
            if let Some(ptr) = store.data() {
                // SAFETY: the store is alive for this borrow and holds exactly `n` bytes.
                unsafe { std::ptr::copy_nonoverlapping(ptr.as_ptr() as *const u8, out.as_mut_ptr(), n) };
            }
        }
        return Ok(out);
    }
    Err("The provided value is not of type '(ArrayBuffer or ArrayBufferView)'.")
}

// ── the encoder ─────────────────────────────────────────────────────────────────────────────────

struct Encoder {
    format: Format,
    deflate: flate2::Compress,
    /// Chrome emits the format header in its OWN chunk, at the first write -- and prepends it to the
    /// finish output when nothing was ever written (measured: `10,23` against a bare `20`).
    header_sent: bool,
    crc: flate2::Crc,
    adler: u32,
}

impl Encoder {
    fn new(format: Format) -> Encoder {
        Encoder {
            format,
            // Level 6 is zlib's default and what Chrome's `78 9c` / `1f8b..00` headers advertise.
            deflate: flate2::Compress::new(flate2::Compression::new(6), false),
            header_sent: false,
            crc: flate2::Crc::new(),
            adler: 1,
        }
    }

    fn checksum(&mut self, bytes: &[u8]) {
        match self.format {
            Format::Gzip => self.crc.update(bytes),
            Format::Deflate => self.adler = adler32(self.adler, bytes),
            Format::DeflateRaw => {}
        }
    }

    /// One `write(chunk)`: the header chunk if it is owed, then every full-or-partial output buffer
    /// the deflater produced.
    fn write(&mut self, mut input: &[u8]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        if !self.header_sent {
            self.header_sent = true;
            let h = self.format.header();
            if !h.is_empty() {
                out.push(h.to_vec());
            }
        }
        self.checksum(input);
        loop {
            let mut buf = vec![0u8; DEFLATE_BUF];
            let (before_in, before_out) = (self.deflate.total_in(), self.deflate.total_out());
            let status = self.deflate.compress(input, &mut buf, flate2::FlushCompress::None);
            let consumed = (self.deflate.total_in() - before_in) as usize;
            let produced = (self.deflate.total_out() - before_out) as usize;
            input = &input[consumed.min(input.len())..];
            if produced > 0 {
                buf.truncate(produced);
                out.push(buf);
            }
            if status.is_err() || (consumed == 0 && produced == 0) || (input.is_empty() && produced < DEFLATE_BUF) {
                break;
            }
        }
        out
    }

    /// `close()`: finish the deflate stream, append the trailer, and hand it back in Chrome's buffer
    /// sizes (one chunk while it fits, which is every case measured).
    fn finish(&mut self) -> Vec<Vec<u8>> {
        let mut tail = Vec::new();
        if !self.header_sent {
            self.header_sent = true;
            tail.extend_from_slice(self.format.header());
        }
        loop {
            let mut buf = vec![0u8; DEFLATE_BUF];
            let before_out = self.deflate.total_out();
            let status = self.deflate.compress(&[], &mut buf, flate2::FlushCompress::Finish);
            let produced = (self.deflate.total_out() - before_out) as usize;
            tail.extend_from_slice(&buf[..produced]);
            match status {
                Ok(flate2::Status::StreamEnd) => break,
                Err(_) => break,
                _ if produced == 0 => break,
                _ => {}
            }
        }
        match self.format {
            Format::Gzip => {
                tail.extend_from_slice(&self.crc.sum().to_le_bytes());
                tail.extend_from_slice(&(self.crc.amount()).to_le_bytes());
            }
            Format::Deflate => tail.extend_from_slice(&self.adler.to_be_bytes()),
            Format::DeflateRaw => {}
        }
        tail.chunks(DEFLATE_BUF).map(|c| c.to_vec()).collect()
    }
}

// ── the decoder ─────────────────────────────────────────────────────────────────────────────────

/// zlib's sentence for a data error, as Chrome prints it. `None` is the detail this backend does not
/// have (see the module header).
fn not_valid(detail: Option<&str>) -> String {
    match detail {
        Some(d) => format!("The compressed data was not valid: {d}."),
        None => "The compressed data was not valid.".to_string(),
    }
}

/// The two malformed-first-block conditions RFC 1951 decides from the bytes alone, which is what
/// zlib is reporting when it says them. Anything deeper needs the inflater's own state.
fn first_block_detail(bits: &[u8]) -> Option<&'static str> {
    let b0 = *bits.first()?;
    match (b0 >> 1) & 3 {
        3 => Some("invalid block type"),
        0 => {
            if bits.len() < 5 {
                return None;
            }
            let len = u16::from_le_bytes([bits[1], bits[2]]);
            let nlen = u16::from_le_bytes([bits[3], bits[4]]);
            (len != !nlen).then_some("invalid stored block lengths")
        }
        _ => None,
    }
}

struct Decoder {
    format: Format,
    inflate: flate2::Decompress,
    /// Bytes of the format header not yet complete (gzip's is variable-length: FEXTRA/FNAME/FCOMMENT).
    head: Vec<u8>,
    head_done: bool,
    /// The inflater reached the end of the deflate stream.
    ended: bool,
    /// Trailer bytes collected since then, and whether they have been checked.
    tail: Vec<u8>,
    tail_checked: bool,
    /// True once nothing at all has been handed to the inflater yet -- the window in which a failure
    /// is a FIRST-BLOCK failure and [`first_block_detail`] can name it.
    first_call: bool,
    crc: flate2::Crc,
    adler: u32,
    out_len: u64,
}

impl Decoder {
    fn new(format: Format) -> Decoder {
        Decoder {
            format,
            inflate: flate2::Decompress::new(false),
            head: Vec::new(),
            head_done: format == Format::DeflateRaw,
            ended: false,
            tail: Vec::new(),
            tail_checked: false,
            first_call: true,
            crc: flate2::Crc::new(),
            adler: 1,
            out_len: 0,
        }
    }

    /// Feed `input` to the format header, which may span several chunks (`gzip` carries a
    /// variable-length FEXTRA/FNAME/FCOMMENT/FHCRC tail, and WPT's `decompression-split-chunk`
    /// delivers one byte at a time). Once it is complete, `self.head` is left holding exactly the
    /// bytes that came AFTER it, for the inflater; while it is not, `self.head` holds what has
    /// arrived so far and `head_done` stays false.
    fn take_header(&mut self, input: &[u8]) -> Result<(), String> {
        self.head.extend_from_slice(input);
        let consumed = match self.format {
            Format::Gzip => match parse_gzip_header(&self.head)? {
                Some(n) => n,
                None => return Ok(()),
            },
            Format::Deflate => match parse_zlib_header(&self.head)? {
                Some(n) => n,
                None => return Ok(()),
            },
            Format::DeflateRaw => 0,
        };
        self.head_done = true;
        self.head = self.head.split_off(consumed);
        Ok(())
    }

    /// One `write(chunk)`: the chunks to enqueue AND, if the input went wrong, the sentence to error
    /// the stream with — in that order, because the bytes that decoded BEFORE the fault are output
    /// Chrome delivers. `decompression-extra-input.any.js` is the shape: a member followed by one
    /// stray byte reads its value first and only the NEXT read rejects.
    fn write(&mut self, input: &[u8]) -> (Vec<Vec<u8>>, Option<String>) {
        let mut out = Vec::new();
        match self.write_inner(input, &mut out) {
            Ok(()) => (out, None),
            Err(e) => (out, Some(e)),
        }
    }

    fn write_inner(&mut self, input: &[u8], out: &mut Vec<Vec<u8>>) -> Result<(), String> {
        let mut pending: Vec<u8>;
        if !self.head_done {
            self.take_header(input)?;
            if !self.head_done {
                return Ok(());
            }
            pending = std::mem::take(&mut self.head);
        } else {
            pending = std::mem::take(&mut self.head);
            pending.extend_from_slice(input);
        }
        let mut rest: &[u8] = &pending;
        while !rest.is_empty() {
            if self.ended {
                return self.take_trailer(rest);
            }
            let mut buf = vec![0u8; INFLATE_BUF];
            let (before_in, before_out) = (self.inflate.total_in(), self.inflate.total_out());
            let status = self.inflate.decompress(rest, &mut buf, flate2::FlushDecompress::None);
            let consumed = (self.inflate.total_in() - before_in) as usize;
            let produced = (self.inflate.total_out() - before_out) as usize;
            let was_first = self.first_call;
            self.first_call = false;
            match status {
                Err(_) => {
                    let detail = if was_first { first_block_detail(rest) } else { None };
                    return Err(not_valid(detail));
                }
                Ok(s) => {
                    if produced > 0 {
                        buf.truncate(produced);
                        self.out_len += produced as u64;
                        match self.format {
                            Format::Gzip => self.crc.update(&buf),
                            Format::Deflate => self.adler = adler32(self.adler, &buf),
                            Format::DeflateRaw => {}
                        }
                        out.push(buf);
                    }
                    rest = &rest[consumed.min(rest.len())..];
                    if s == flate2::Status::StreamEnd {
                        self.ended = true;
                        continue;
                    }
                    if consumed == 0 && produced == 0 {
                        // No progress with input still in hand: keep it for the next chunk rather
                        // than dropping it on the floor.
                        self.head = rest.to_vec();
                        break;
                    }
                }
            }
        }
        if self.ended {
            self.take_trailer(&[])?;
        }
        Ok(())
    }

    /// The bytes after the deflate stream: the format's trailer, then NOTHING (measured -- one stray
    /// byte, or a whole second gzip member, is `Junk found after end of compressed data.`).
    fn take_trailer(&mut self, input: &[u8]) -> Result<(), String> {
        let want = self.format.trailer_len();
        let take = want.saturating_sub(self.tail.len()).min(input.len());
        self.tail.extend_from_slice(&input[..take]);
        if self.tail.len() == want && !self.tail_checked {
            self.tail_checked = true;
            match self.format {
                Format::Gzip => {
                    let crc = u32::from_le_bytes([self.tail[0], self.tail[1], self.tail[2], self.tail[3]]);
                    if crc != self.crc.sum() {
                        return Err(not_valid(Some("incorrect data check")));
                    }
                    let isize_ = u32::from_le_bytes([self.tail[4], self.tail[5], self.tail[6], self.tail[7]]);
                    if isize_ != (self.out_len & 0xffff_ffff) as u32 {
                        return Err(not_valid(Some("incorrect length check")));
                    }
                }
                Format::Deflate => {
                    let a = u32::from_be_bytes([self.tail[0], self.tail[1], self.tail[2], self.tail[3]]);
                    if a != self.adler {
                        return Err(not_valid(Some("incorrect data check")));
                    }
                }
                Format::DeflateRaw => {}
            }
        }
        if take < input.len() {
            return Err("Junk found after end of compressed data.".to_string());
        }
        Ok(())
    }

    /// `close()`. `Err` is the sentence to error the stream with.
    fn finish(&mut self) -> Result<(), String> {
        if !self.ended || self.tail.len() < self.format.trailer_len() {
            return Err("Compressed input was truncated.".to_string());
        }
        Ok(())
    }
}

/// RFC 1952 §2.3. `Ok(Some(n))` = the header is `n` bytes; `Ok(None)` = it wants more.
fn parse_gzip_header(b: &[u8]) -> Result<Option<usize>, String> {
    if b.len() < 2 {
        // Not enough to judge -- but the first byte can already disagree with the magic.
        if !b.is_empty() && b[0] != 0x1f {
            return Err(not_valid(Some("incorrect header check")));
        }
        return Ok(None);
    }
    if b[0] != 0x1f || b[1] != 0x8b {
        return Err(not_valid(Some("incorrect header check")));
    }
    if b.len() < 3 {
        return Ok(None);
    }
    if b[2] != 8 {
        return Err(not_valid(Some("unknown compression method")));
    }
    if b.len() < 4 {
        return Ok(None);
    }
    let flg = b[3];
    if flg & 0xe0 != 0 {
        return Err(not_valid(Some("unknown header flags set")));
    }
    if b.len() < 10 {
        return Ok(None);
    }
    let mut i = 10usize;
    if flg & 0x04 != 0 {
        // FEXTRA
        if b.len() < i + 2 {
            return Ok(None);
        }
        let xlen = u16::from_le_bytes([b[i], b[i + 1]]) as usize;
        i += 2 + xlen;
        if b.len() < i {
            return Ok(None);
        }
    }
    for flag in [0x08u8, 0x10] {
        // FNAME, FCOMMENT: NUL-terminated
        if flg & flag != 0 {
            match b[i..].iter().position(|&c| c == 0) {
                Some(p) => i += p + 1,
                None => return Ok(None),
            }
        }
    }
    if flg & 0x02 != 0 {
        // FHCRC: the low 16 bits of the CRC32 of the header so far
        if b.len() < i + 2 {
            return Ok(None);
        }
        let mut crc = flate2::Crc::new();
        crc.update(&b[..i]);
        let want = u16::from_le_bytes([b[i], b[i + 1]]);
        if want != (crc.sum() & 0xffff) as u16 {
            return Err(not_valid(Some("header crc mismatch")));
        }
        i += 2;
    }
    Ok(Some(i))
}

/// RFC 1950 §2.2, in zlib's own check order: FCHECK first, then CM, then CINFO.
fn parse_zlib_header(b: &[u8]) -> Result<Option<usize>, String> {
    if b.len() < 2 {
        return Ok(None);
    }
    let (cmf, flg) = (b[0] as u32, b[1] as u32);
    if ((cmf << 8) + flg) % 31 != 0 {
        return Err(not_valid(Some("incorrect header check")));
    }
    if cmf & 0x0f != 8 {
        return Err(not_valid(Some("unknown compression method")));
    }
    if (cmf >> 4) + 8 > 15 {
        return Err(not_valid(Some("invalid window size")));
    }
    if flg & 0x20 != 0 {
        // FDICT: zlib answers Z_NEED_DICT, which DecompressionStream has no way to satisfy.
        return Err(not_valid(Some("need dictionary")));
    }
    Ok(Some(2))
}

// ── the two cppgc classes ───────────────────────────────────────────────────────────────────────

#[repr(C)]
#[derive(CppgcBase)]
pub struct CompressionStream {
    readable: std::cell::RefCell<Option<v8::Global<v8::Object>>>,
    writable: std::cell::RefCell<Option<v8::Global<v8::Object>>>,
    enc: std::cell::RefCell<Option<Encoder>>,
}

pub(crate) trait CompressionStreamBehavior {
    fn construct() -> CompressionStream where Self: Sized { stub!("CompressionStream::constructor") }
}

#[webapi(base, own)]
impl CompressionStream {
    #[constructor]
    #[cppgc]
    fn constructor() -> Result<CompressionStream, ::deno_error::JsErrorBox> {
        dispatch!("CompressionStream::constructor", <CompressionStream as CompressionStreamBehavior>::construct())
    }
}

impl CompressionStreamBehavior for CompressionStream {
    fn construct() -> CompressionStream {
        CompressionStream {
            readable: std::cell::RefCell::new(None),
            writable: std::cell::RefCell::new(None),
            enc: std::cell::RefCell::new(None),
        }
    }
}

#[repr(C)]
#[derive(CppgcBase)]
pub struct DecompressionStream {
    readable: std::cell::RefCell<Option<v8::Global<v8::Object>>>,
    writable: std::cell::RefCell<Option<v8::Global<v8::Object>>>,
    dec: std::cell::RefCell<Option<Decoder>>,
}

pub(crate) trait DecompressionStreamBehavior {
    fn construct() -> DecompressionStream where Self: Sized { stub!("DecompressionStream::constructor") }
}

#[webapi(base, own)]
impl DecompressionStream {
    #[constructor]
    #[cppgc]
    fn constructor() -> Result<DecompressionStream, ::deno_error::JsErrorBox> {
        dispatch!("DecompressionStream::constructor", <DecompressionStream as DecompressionStreamBehavior>::construct())
    }
}

impl DecompressionStreamBehavior for DecompressionStream {
    fn construct() -> DecompressionStream {
        DecompressionStream {
            readable: std::cell::RefCell::new(None),
            writable: std::cell::RefCell::new(None),
            dec: std::cell::RefCell::new(None),
        }
    }
}


// ── shared plumbing ─────────────────────────────────────────────────────────────────────────────

fn adopt_proto(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, obj: v8::Local<v8::Object>, iface: &str) {
    let proto = v8::Local::<v8::Object>::try_from(args.new_target())
        .ok()
        .and_then(|nt| get_prop(scope, nt, "prototype"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    match proto {
        Some(p) => {
            obj.set_prototype(scope, p.into());
        }
        None => set_table_proto(scope, obj, iface),
    }
}

/// A `readable` + a `writable` whose sink's `write`/`close` carry `owner` as their bound data -- the
/// same pair `TextDecoderStream` builds, from the same two stream types.
fn make_pair<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    owner: v8::Local<'s, v8::Object>,
    write: impl v8::MapFnTo<v8::FunctionCallback>,
    close: impl v8::MapFnTo<v8::FunctionCallback>,
) -> (v8::Local<'s, v8::Object>, v8::Local<'s, v8::Object>) {
    let readable = super::readable_stream::new_stream(scope);
    let sink = v8::Object::new(scope);
    let ov: v8::Local<v8::Value> = owner.into();
    let w = build_fn_data(scope, write, "write", 1, ov);
    let c = build_fn_data(scope, close, "close", 0, ov);
    set_prop(scope, sink, "write", w.into());
    set_prop(scope, sink, "close", c.into());
    let writable = crate::binding::class::mint(
        scope,
        <super::writable_stream::WritableStream as super::writable_stream::WritableStreamBehavior>::construct(),
    );
    set_table_proto(scope, writable, "WritableStream");
    super::writable_stream::ws_init(scope, writable.into(), sink.into());
    (readable, writable)
}

/// Error `readable` with a new `TypeError(msg)` and throw the same object: the write rejects with it
/// and so does every read, which is what Chrome answers on both halves.
fn fail(scope: &mut v8::PinScope, readable: Option<v8::Global<v8::Object>>, msg: &str) {
    let m = v8::String::new(scope, msg).unwrap();
    let exc = v8::Exception::type_error(scope, m);
    if let Some(r) = readable {
        let r = v8::Local::new(scope, &r);
        rs_error(scope, r.into(), exc);
    }
    scope.throw_exception(exc);
}

/// A `Uint8Array` owning `bytes` -- the chunk type both readables enqueue (measured: every output
/// chunk is `[object Uint8Array]`).
fn u8_array<'s>(scope: &mut v8::PinScope<'s, '_>, bytes: Vec<u8>) -> v8::Local<'s, v8::Value> {
    let len = bytes.len();
    let store = v8::ArrayBuffer::new_backing_store_from_vec(bytes).make_shared();
    let ab = v8::ArrayBuffer::with_backing_store(scope, &store);
    match v8::Uint8Array::new(scope, ab, 0, len) {
        Some(a) => a.into(),
        None => v8::undefined(scope).into(),
    }
}

/// `format` as Chrome reads it. `None` means the scope already holds the exception to propagate.
///
/// The Symbol branch is not a special case invented here: Blink's string conversion refuses a Symbol
/// THROUGH its `ExceptionState`, which is the object that prefixes a message with the construction
/// context, while every other failure of `ToString` is the page's own exception travelling outwards
/// untouched. Measured, and the two halves of that rule disagree exactly where the mechanism says
/// they should: `Symbol()` -> `TypeError: Failed to construct 'CompressionStream': Cannot convert a
/// Symbol value to a string`, but `{toString(){throw new TypeError('mine')}}` -> `TypeError: mine`
/// with no prefix at all -- SAME error type, so a rule written from the type would get it wrong.
fn format_arg(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, iface: &str) -> Option<Format> {
    if args.length() < 1 {
        throw_type_error(scope, &format!("Failed to construct '{iface}': 1 argument required, but only 0 present."));
        return None;
    }
    let arg = args.get(0);
    if arg.is_symbol() {
        throw_type_error(scope, &format!("Failed to construct '{iface}': Cannot convert a Symbol value to a string"));
        return None;
    }
    // Every other value is stringified, so `undefined` is the spelling `'undefined'` and `1` is `'1'`.
    let s = arg.to_string(scope)?.to_rust_string_lossy(scope);
    match Format::parse(&s) {
        Some(f) => Some(f),
        None => {
            throw_type_error(scope, &format!("Failed to construct '{iface}': Unsupported compression format: '{s}'"));
            None
        }
    }
}

// ── CompressionStream bodies ────────────────────────────────────────────────────────────────────

fn cs_of<'a>(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<&'a CompressionStream> {
    deno_core::cppgc::try_unwrap_cppgc_object::<CompressionStream>(scope, v)
        .map(|p| unsafe { &*(p.as_ref() as *const CompressionStream) })
}

fn emit(scope: &mut v8::PinScope, readable: &Option<v8::Global<v8::Object>>, chunks: Vec<Vec<u8>>) {
    let Some(r) = readable.as_ref() else { return };
    let r = v8::Local::new(scope, r);
    for c in chunks {
        let v = u8_array(scope, c);
        let _ = rs_enqueue(scope, r.into(), v);
    }
}

fn cb_cs_sink_write(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some(cs) = cs_of(scope, __body_data) else { return };
    let readable = cs.readable.borrow().clone();
    let bytes = match chunk_bytes(scope, args.get(0)) {
        Ok(b) => b,
        Err(msg) => {
            fail(scope, readable, msg);
            return;
        }
    };
    let chunks = {
        let mut slot = cs.enc.borrow_mut();
        match slot.as_mut() {
            Some(e) => e.write(&bytes),
            None => return,
        }
    };
    emit(scope, &readable, chunks);
}

fn cb_cs_sink_close(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some(cs) = cs_of(scope, __body_data) else { return };
    let readable = cs.readable.borrow().clone();
    let chunks = {
        let mut slot = cs.enc.borrow_mut();
        match slot.as_mut() {
            Some(e) => e.finish(),
            None => return,
        }
    };
    emit(scope, &readable, chunks);
    if let Some(r) = readable {
        let r = v8::Local::new(scope, &r);
        let _ = rs_close(scope, r.into());
    }
}

/// `new CompressionStream(format)`.
pub fn cb_cs_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !args.new_target().is_object() {
        throw_type_error(scope, &crate::binding::protos::use_new_msg("CompressionStream"));
        return;
    }
    let Some(format) = format_arg(scope, &args, "CompressionStream") else { return };
    let obj = crate::binding::class::mint(scope, <CompressionStream as CompressionStreamBehavior>::construct());
    adopt_proto(scope, &args, obj, "CompressionStream");
    let (readable, writable) = make_pair(scope, obj, cb_cs_sink_write, cb_cs_sink_close);
    if let Some(cs) = cs_of(scope, obj.into()) {
        *cs.enc.borrow_mut() = Some(Encoder::new(format));
        *cs.readable.borrow_mut() = Some(v8::Global::new(scope, readable));
        *cs.writable.borrow_mut() = Some(v8::Global::new(scope, writable));
    }
    rv.set(obj.into());
}

fn cs_this<'a>(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> Option<&'a CompressionStream> {
    let t = cs_of(scope, args.this().into());
    if t.is_none() {
        throw_type_error(scope, "Illegal invocation");
    }
    t
}

fn cb_cs_readable(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(t) = cs_this(scope, &args) else { return };
    if let Some(g) = t.readable.borrow().as_ref() {
        rv.set(v8::Local::new(scope, g).into());
    }
}

fn cb_cs_writable(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(t) = cs_this(scope, &args) else { return };
    if let Some(g) = t.writable.borrow().as_ref() {
        rv.set(v8::Local::new(scope, g).into());
    }
}

/// The body every row of [`COMPRESSIONSTREAM`] names.
pub fn install_compression_stream(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "readable" => getter_row(scope, proto, member, cb_cs_readable),
        "writable" => getter_row(scope, proto, member, cb_cs_writable),
        other => debug_assert!(false, "install_compression_stream: unexpected member {other}"),
    }
}

// ── DecompressionStream bodies ──────────────────────────────────────────────────────────────────

fn ds_of<'a>(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<&'a DecompressionStream> {
    deno_core::cppgc::try_unwrap_cppgc_object::<DecompressionStream>(scope, v)
        .map(|p| unsafe { &*(p.as_ref() as *const DecompressionStream) })
}

fn cb_ds_sink_write(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some(ds) = ds_of(scope, __body_data) else { return };
    let readable = ds.readable.borrow().clone();
    let bytes = match chunk_bytes(scope, args.get(0)) {
        Ok(b) => b,
        Err(msg) => {
            fail(scope, readable, msg);
            return;
        }
    };
    let (chunks, err) = {
        let mut slot = ds.dec.borrow_mut();
        match slot.as_mut() {
            Some(d) => d.write(&bytes),
            None => return,
        }
    };
    emit(scope, &readable, chunks);
    if let Some(msg) = err {
        fail(scope, readable, &msg);
    }
}

fn cb_ds_sink_close(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some(ds) = ds_of(scope, __body_data) else { return };
    let readable = ds.readable.borrow().clone();
    let done = {
        let mut slot = ds.dec.borrow_mut();
        match slot.as_mut() {
            Some(d) => d.finish(),
            None => return,
        }
    };
    if let Err(msg) = done {
        fail(scope, readable, &msg);
        return;
    }
    if let Some(r) = readable {
        let r = v8::Local::new(scope, &r);
        let _ = rs_close(scope, r.into());
    }
}

/// `new DecompressionStream(format)`.
pub fn cb_ds_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !args.new_target().is_object() {
        throw_type_error(scope, &crate::binding::protos::use_new_msg("DecompressionStream"));
        return;
    }
    let Some(format) = format_arg(scope, &args, "DecompressionStream") else { return };
    let obj = crate::binding::class::mint(scope, <DecompressionStream as DecompressionStreamBehavior>::construct());
    adopt_proto(scope, &args, obj, "DecompressionStream");
    let (readable, writable) = make_pair(scope, obj, cb_ds_sink_write, cb_ds_sink_close);
    if let Some(ds) = ds_of(scope, obj.into()) {
        *ds.dec.borrow_mut() = Some(Decoder::new(format));
        *ds.readable.borrow_mut() = Some(v8::Global::new(scope, readable));
        *ds.writable.borrow_mut() = Some(v8::Global::new(scope, writable));
    }
    rv.set(obj.into());
}

fn ds_this<'a>(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> Option<&'a DecompressionStream> {
    let t = ds_of(scope, args.this().into());
    if t.is_none() {
        throw_type_error(scope, "Illegal invocation");
    }
    t
}

fn cb_ds_readable(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(t) = ds_this(scope, &args) else { return };
    if let Some(g) = t.readable.borrow().as_ref() {
        rv.set(v8::Local::new(scope, g).into());
    }
}

fn cb_ds_writable(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(t) = ds_this(scope, &args) else { return };
    if let Some(g) = t.writable.borrow().as_ref() {
        rv.set(v8::Local::new(scope, g).into());
    }
}

/// The body every row of [`DECOMPRESSIONSTREAM`] names.
pub fn install_decompression_stream(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "readable" => getter_row(scope, proto, member, cb_ds_readable),
        "writable" => getter_row(scope, proto, member, cb_ds_writable),
        other => debug_assert!(false, "install_decompression_stream: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `CompressionStream` / `DecompressionStream` blocks of
// `install/generated_registry.rs` (I305 / I300). `[value, both]`: every bundle records both as newable
// in both realms, constructor length 1.
webapi::interface! {
    "CompressionStream" [value, both] {
        ctor "CompressionStream"           len 1 = stub,
        attr "readable" : Any readonly           = install_compression_stream,
        attr "writable" : Any readonly           = install_compression_stream
    }
}

webapi::interface! {
    "DecompressionStream" [value, both] {
        ctor "DecompressionStream"         len 1 = stub,
        attr "readable" : Any readonly           = install_decompression_stream,
        attr "writable" : Any readonly           = install_decompression_stream
    }
}
