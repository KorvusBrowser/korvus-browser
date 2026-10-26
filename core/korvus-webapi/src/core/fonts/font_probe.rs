//! `core/fonts/font_probe.rs` -- is a font body a font?
//!
//! `FontFace.load()` resolves once its bytes parse as a font and rejects with `NetworkError` when they
//! do not; Chrome decides with OTS. This is the same question asked of the container, not of every
//! glyph: an sfnt (TrueType / OpenType / a collection), a WOFF (zlib tables) or a WOFF2 (one Brotli
//! stream) whose directory fits the file, whose compressed data inflates to the sizes it declares, and
//! which carries the tables every font needs (`head` with its magic number, `cmap`, `hhea`, `hmtx`,
//! `maxp`). Measured on stock Chrome 150 (tools/realweb/seed/fontface): a real TrueType font loads,
//! and the 128-byte WOFF2 header the parity fixtures carry -- right magic, a 64 KiB compressed stream
//! in a 128-byte file -- is rejected.
//!
//! The kernel's subresource loader holds the bytes when a `font` response arrives and records the
//! answer in [`FontProbes`] under the request URL; the page's realm never receives the body. A body
//! that is a font is kept beside its answer ([`FontProbes::keep_body`]): the document's painter draws
//! the page's `@font-face` text in it (`cssom::complete_fonts`).

use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::sync::{Arc, Mutex};

const REQUIRED: [&[u8; 4]; 5] = [b"head", b"cmap", b"hhea", b"hmtx", b"maxp"];
const HEAD_MAGIC: u32 = 0x5F0F_3CF5;

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(i..i + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

/// Does `bytes` parse as a font?
pub fn probe(bytes: &[u8]) -> bool {
    match bytes.get(..4) {
        Some(b"wOFF") => woff(bytes).is_some(),
        Some(b"wOF2") => woff2(bytes).is_some(),
        Some(b"ttcf") => collection(bytes).is_some(),
        Some(_) => sfnt(bytes, 0).is_some(),
        None => false,
    }
}

/// The tables found, each `(tag, bytes)`, must hold the required set and a `head` with its magic.
fn complete(tables: &[([u8; 4], &[u8])]) -> Option<()> {
    for req in REQUIRED {
        tables.iter().find(|(t, _)| t == req)?;
    }
    let head = tables.iter().find(|(t, _)| t == b"head")?.1;
    (u32_at(head, 12)? == HEAD_MAGIC).then_some(())
}

/// An sfnt's offset table at `at` (TrueType `0x00010000` / `true`, OpenType `OTTO`).
fn sfnt(b: &[u8], at: usize) -> Option<()> {
    let version = u32_at(b, at)?;
    if !matches!(version, 0x0001_0000 | 0x7472_7565 | 0x4F54_544F) {
        return None;
    }
    let n = u16_at(b, at + 4)? as usize;
    if n == 0 || n > 1024 {
        return None;
    }
    let mut tables = Vec::with_capacity(n);
    for i in 0..n {
        let rec = at + 12 + 16 * i;
        let tag: [u8; 4] = b.get(rec..rec + 4)?.try_into().ok()?;
        let off = u32_at(b, rec + 8)? as usize;
        let len = u32_at(b, rec + 12)? as usize;
        tables.push((tag, b.get(off..off.checked_add(len)?)?));
    }
    complete(&tables)
}

/// A TrueType collection: its first font must be one.
fn collection(b: &[u8]) -> Option<()> {
    let fonts = u32_at(b, 8)?;
    if fonts == 0 {
        return None;
    }
    sfnt(b, u32_at(b, 12)? as usize)
}

/// WOFF 1.0: a directory of zlib-compressed (or stored) sfnt tables.
fn woff(b: &[u8]) -> Option<()> {
    if u32_at(b, 8)? as usize != b.len() {
        return None;
    }
    let n = u16_at(b, 12)? as usize;
    if n == 0 || n > 1024 {
        return None;
    }
    let mut owned: Vec<([u8; 4], Vec<u8>)> = Vec::with_capacity(n);
    for i in 0..n {
        let rec = 44 + 20 * i;
        let tag: [u8; 4] = b.get(rec..rec + 4)?.try_into().ok()?;
        let off = u32_at(b, rec + 4)? as usize;
        let comp = u32_at(b, rec + 8)? as usize;
        let orig = u32_at(b, rec + 12)? as usize;
        let data = b.get(off..off.checked_add(comp)?)?;
        if comp > orig {
            return None;
        }
        let table = if comp == orig {
            data.to_vec()
        } else {
            let mut out = Vec::with_capacity(orig);
            flate2::read::ZlibDecoder::new(data).take(orig as u64 + 1).read_to_end(&mut out).ok()?;
            if out.len() != orig {
                return None;
            }
            out
        };
        owned.push((tag, table));
    }
    let tables: Vec<([u8; 4], &[u8])> = owned.iter().map(|(t, d)| (*t, d.as_slice())).collect();
    complete(&tables)
}

/// WOFF2's known-table index (the 63 tags a directory entry can name by number).
const WOFF2_TAGS: [&[u8; 4]; 63] = [
    b"cmap", b"head", b"hhea", b"hmtx", b"maxp", b"name", b"OS/2", b"post", b"cvt ", b"fpgm", b"glyf", b"loca", b"prep",
    b"CFF ", b"VORG", b"EBDT", b"EBLC", b"gasp", b"hdmx", b"kern", b"LTSH", b"PCLT", b"VDMX", b"vhea", b"vmtx", b"BASE",
    b"GDEF", b"GPOS", b"GSUB", b"EBSC", b"JSTF", b"MATH", b"CBDT", b"CBLC", b"COLR", b"CPAL", b"SVG ", b"sbix", b"acnt",
    b"avar", b"bdat", b"bloc", b"bsln", b"cvar", b"fdsc", b"feat", b"fmtx", b"fvar", b"gvar", b"hsty", b"just", b"lcar",
    b"mort", b"morx", b"opbd", b"prop", b"trak", b"Zapf", b"Silf", b"Glat", b"Gloc", b"Feat", b"Sill",
];

fn base128(b: &[u8], at: &mut usize) -> Option<u32> {
    let mut v: u32 = 0;
    for i in 0..5 {
        let byte = *b.get(*at)?;
        *at += 1;
        if i == 0 && byte == 0x80 {
            return None;
        }
        if v & 0xFE00_0000 != 0 {
            return None;
        }
        v = (v << 7) | (byte & 0x7F) as u32;
        if byte & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

fn u255(b: &[u8], at: &mut usize) -> Option<u32> {
    let code = *b.get(*at)?;
    *at += 1;
    Some(match code {
        253 => {
            let v = u16_at(b, *at)? as u32;
            *at += 2;
            v
        }
        255 => {
            let v = *b.get(*at)? as u32 + 253;
            *at += 1;
            v
        }
        254 => {
            let v = *b.get(*at)? as u32 + 506;
            *at += 1;
            v
        }
        c => c as u32,
    })
}

/// WOFF 2.0: a table directory, then ONE Brotli stream holding every table (some transformed).
fn woff2(b: &[u8]) -> Option<()> {
    if u32_at(b, 8)? as usize != b.len() {
        return None;
    }
    let flavor = u32_at(b, 4)?;
    let n = u16_at(b, 12)? as usize;
    let compressed = u32_at(b, 20)? as usize;
    if n == 0 || n > 1024 {
        return None;
    }
    let mut at = 48;
    // (tag, stored length, transformed?)
    let mut dir: Vec<([u8; 4], usize, bool)> = Vec::with_capacity(n);
    for _ in 0..n {
        let flags = *b.get(at)?;
        at += 1;
        let tag: [u8; 4] = if flags & 0x3F == 0x3F {
            let t = b.get(at..at + 4)?.try_into().ok()?;
            at += 4;
            t
        } else {
            **WOFF2_TAGS.get((flags & 0x3F) as usize)?
        };
        let orig = base128(b, &mut at)? as usize;
        let version = flags >> 6;
        let glyf_loca = &tag == b"glyf" || &tag == b"loca";
        let transformed = if glyf_loca { version == 0 } else { version != 0 };
        let stored = if transformed { base128(b, &mut at)? as usize } else { orig };
        dir.push((tag, stored, transformed));
    }
    if flavor == 0x7474_6366 {
        // A collection directory: the fonts' table indices, read to find where the stream begins.
        at += 4;
        let fonts = u255(b, &mut at)?;
        for _ in 0..fonts {
            let tables = u255(b, &mut at)?;
            at += 4;
            for _ in 0..tables {
                u255(b, &mut at)?;
            }
        }
    }
    let stream = b.get(at..at.checked_add(compressed)?)?;
    let total: usize = dir.iter().map(|(_, len, _)| *len).sum();
    let mut out = Vec::with_capacity(total.min(64 << 20));
    brotli_decompressor::Decompressor::new(stream, 4096).take(total as u64 + 1).read_to_end(&mut out).ok()?;
    if out.len() != total {
        return None;
    }
    let mut tables: Vec<([u8; 4], &[u8])> = Vec::with_capacity(n);
    let mut off = 0;
    for (tag, len, _) in &dir {
        tables.push((*tag, &out[off..off + len]));
        off += len;
    }
    complete(&tables)
}

/// The answers for the font bodies this realm fetched, by request URL (see the module header).
#[derive(Clone, Default)]
pub struct FontProbes(Arc<Mutex<ProbeList>>);

#[derive(Default)]
struct ProbeList {
    map: HashMap<String, bool>,
    order: VecDeque<String>,
    /// The bodies that are fonts, by request URL, and their bytes together.
    bodies: HashMap<String, Arc<Vec<u8>>>,
    body_bytes: usize,
    /// Moves whenever a body is kept.
    generation: u64,
}

impl FontProbes {
    /// How many answers are kept. An answer is read once, moments after it is written.
    const KEEP: usize = 256;

    /// How many bytes of font bodies are kept: the oldest go first.
    const KEEP_BYTES: usize = 64 << 20;

    pub fn insert(&self, url: &str, ok: bool) {
        let Ok(mut l) = self.0.lock() else { return };
        if l.map.insert(url.to_string(), ok).is_none() {
            l.order.push_back(url.to_string());
        }
        while l.order.len() > Self::KEEP {
            if let Some(k) = l.order.pop_front() {
                l.map.remove(&k);
                if let Some(b) = l.bodies.remove(&k) {
                    l.body_bytes -= b.len();
                }
            }
        }
    }

    /// Keep the body fetched for `url`, which [`probe`] found a font, for drawing in (see the module header).
    pub fn keep_body(&self, url: &str, bytes: &[u8]) {
        let Ok(mut l) = self.0.lock() else { return };
        if bytes.len() > Self::KEEP_BYTES {
            return;
        }
        let l = &mut *l;
        if let Some(old) = l.bodies.insert(url.to_string(), Arc::new(bytes.to_vec())) {
            l.body_bytes -= old.len();
        }
        l.body_bytes += bytes.len();
        l.generation += 1;
        let mut i = 0;
        while l.body_bytes > Self::KEEP_BYTES && i < l.order.len() {
            if l.order[i] != url {
                if let Some(b) = l.bodies.remove(&l.order[i]) {
                    l.body_bytes -= b.len();
                }
            }
            i += 1;
        }
    }

    /// A number that moves whenever a font body is kept.
    pub fn generation(&self) -> u64 {
        self.0.lock().map_or(0, |l| l.generation)
    }

    /// The font body kept for `url`, if any.
    pub fn body(&self, url: &str) -> Option<Arc<Vec<u8>>> {
        self.0.lock().ok()?.bodies.get(url).cloned()
    }

    /// `None`: no answer was recorded for `url`.
    pub fn get(&self, url: &str) -> Option<bool> {
        self.0.lock().ok()?.map.get(url).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::probe;

    #[test]
    fn a_font_is_a_font_and_a_header_is_not() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/realweb/seed/fontface/");
        let ttf = std::fs::read(format!("{root}f.ttf")).expect("fixture font");
        assert!(probe(&ttf), "NotoSansLycian is a TrueType font");
        let stub = std::fs::read(format!("{root}stub.woff2")).expect("fixture stub");
        assert!(!probe(&stub), "a 128-byte woff2 header declaring a 64 KiB stream is not a font");
        assert!(!probe(b""));
        assert!(!probe(&[1, 2, 3, 4]));
        assert!(!probe(&ttf[..ttf.len() / 2]), "a truncated font is not a font");
    }
}
