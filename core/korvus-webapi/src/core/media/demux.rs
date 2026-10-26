//! `core/media/demux.rs` -- a media resource's TRACKS and SAMPLES: what a `<video>` decodes and what a `SourceBuffer`
//! buffers.
//!
//! `media_probe.rs` reads a header's three facts; this reads the rest. Two containers, the two MSE byte streams
//! Chrome takes and the two a `<video src>` serves in practice:
//!
//! * ISO BMFF -- a progressive MP4 (`moov` sample tables over an `mdat`) and a fragmented one (an init segment's
//!   `moov`, then `moof` + `mdat` media segments; MSE's `video/mp4`).
//! * WebM -- the EBML header, `Segment`, `Info`, `Tracks`, then `Cluster`s of blocks (MSE's `video/webm`).
//!
//! THE TIMES ARE CHROME'S. A track's edit list moves its samples, as Chromium's `MP4StreamParser` applies the first
//! edit's `media_time` (a B-frame stream's composition offsets start a frame or two in, and the edit takes the first
//! frame to 0); a WebM block's time is its cluster's plus its own, in the segment's `TimecodeScale`. Every time leaves
//! here in seconds.
//!
//! [`Parser`] takes bytes as they come -- an `appendBuffer` can end in the middle of a box -- and keeps what it cannot
//! read yet for the next call; [`movie`] is the same parse over a whole file (a progressive MP4's sample tables are
//! read where they are, since its `moov` can follow its `mdat`).

use std::sync::Arc;

/// A run of bytes inside a buffer shared by every sample cut from it.
#[derive(Clone)]
pub struct Bytes {
    buf: Arc<Vec<u8>>,
    start: usize,
    len: usize,
}

impl Bytes {
    /// All of `buf`.
    #[cfg(test)]
    pub fn whole(buf: Arc<Vec<u8>>) -> Bytes {
        let len = buf.len();
        Bytes { buf, start: 0, len }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.buf[self.start..self.start + self.len]
    }
}

impl std::fmt::Debug for Bytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Bytes({})", self.len)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Video,
    Audio,
    Other,
}

/// One track of an init segment (or a whole file).
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub id: u64,
    pub kind: Kind,
    /// The sample format: four-cc and configuration record (`avcC`, `av1C`, ...); a WebM codec ID is mapped to the
    /// ISO four-cc of the same format.
    pub codec: spec::VideoCodec,
    pub width: u32,
    pub height: u32,
    /// MP4: the media timescale; WebM: unused.
    timescale: u32,
    /// MP4: ticks the edit list moves each sample by (the first edit's `media_time`, less any empty edit before it).
    shift: i64,
    /// A sample's duration when nothing else gives one: `trex`'s, or WebM's `DefaultDuration`, in seconds.
    default_duration: f64,
    /// `trex`'s defaults: sample size and flags.
    default_size: u32,
    default_flags: u32,
}

/// What an init segment (or a whole file's header) declares.
#[derive(Clone, Debug, PartialEq)]
pub struct Init {
    pub tracks: Vec<Track>,
    /// The duration the header states, in seconds, if it states one.
    pub duration: Option<f64>,
}

/// One coded frame.
#[derive(Clone, Debug)]
pub struct Sample {
    pub track: u64,
    pub dts: f64,
    pub pts: f64,
    pub duration: f64,
    /// A random access point: decoding can start here.
    pub key: bool,
    /// The coded bytes -- kept for video only; an audio frame's place in time is all a buffer needs of it.
    pub data: Option<Bytes>,
}

/// What a run of bytes held.
#[derive(Debug)]
pub enum Event {
    Init(Init),
    Samples(Vec<Sample>),
}

/// The bytes are not the format, or contradict themselves: MSE's append error (`decode` end of stream).
#[derive(Debug, PartialEq, Eq)]
pub struct ParseError;

/// `ticks / scale` seconds, cut to the microsecond as Chromium's `TimeDeltaFromRational` cuts it -- each frame's
/// time and duration on its own, so a buffered end is their sum (Chrome 150: WPT's 24 fps MSE clip ends at 2.083332).
fn secs(ticks: i64, scale: u32) -> f64 {
    ((ticks as i128 * 1_000_000) / scale.max(1) as i128) as f64 / 1e6
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn be64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

// ---- ISO BMFF -------------------------------------------------------------------------------------

/// A box's header at `at`: (type, header length, total length). `None` until the header is all here; a size-0 box
/// runs to the end of `b` only when `to_end` says the buffer is the whole file.
fn box_header(b: &[u8], at: usize, to_end: bool) -> Option<([u8; 4], usize, usize)> {
    let size = be32(b, at)? as u64;
    let ty: [u8; 4] = b.get(at + 4..at + 8)?.try_into().ok()?;
    let (header, size) = match size {
        1 => (16, be64(b, at + 8)?),
        0 if to_end => (8, (b.len() - at) as u64),
        0 => return None,
        n => (8, n),
    };
    if size < header as u64 || size > usize::MAX as u64 / 2 {
        return Some((ty, header, header));
    }
    Some((ty, header, size as usize))
}

/// The boxes directly inside `b` as (type, body offset in `b`, body).
fn boxes(b: &[u8]) -> impl Iterator<Item = ([u8; 4], usize, &[u8])> {
    let mut at = 0usize;
    std::iter::from_fn(move || {
        let (ty, header, size) = box_header(b, at, true)?;
        let end = at.checked_add(size)?.min(b.len());
        let body_at = (at + header).min(end);
        let body = &b[body_at..end];
        at = end.max(at + header);
        Some((ty, body_at, body))
    })
}

fn child<'a>(b: &'a [u8], ty: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(b).find(|(t, _, _)| t == ty).map(|(_, _, body)| body)
}

fn path<'a>(b: &'a [u8], tys: &[&[u8; 4]]) -> Option<&'a [u8]> {
    tys.iter().try_fold(b, |b, t| child(b, t))
}

/// A `moov`'s tracks and stated duration.
fn mp4_init(moov: &[u8]) -> Option<Init> {
    let mvhd = child(moov, b"mvhd")?;
    let (movie_scale, movie_duration) = header_duration(mvhd)?;
    // a fragmented file states its length in `mehd`, a progressive one in `mvhd`
    let mehd = path(moov, &[b"mvex", b"mehd"]).and_then(|m| if *m.first()? == 1 { be64(m, 4) } else { be32(m, 4).map(u64::from) });
    let stated = mehd.filter(|d| *d > 0).or(Some(movie_duration).filter(|d| *d > 0));
    let duration = stated.filter(|_| movie_scale > 0).map(|d| d as f64 / movie_scale as f64);
    let mut tracks = Vec::new();
    for (_, _, trak) in boxes(moov).filter(|(t, _, _)| t == b"trak") {
        if let Some(t) = mp4_track(trak, movie_scale) {
            tracks.push(t);
        }
    }
    // `trex`: the defaults a fragment's samples take
    if let Some(mvex) = child(moov, b"mvex") {
        for (_, _, trex) in boxes(mvex).filter(|(t, _, _)| t == b"trex") {
            let Some(id) = be32(trex, 4) else { continue };
            if let Some(t) = tracks.iter_mut().find(|t| t.id == id as u64) {
                let duration = be32(trex, 12).unwrap_or(0);
                t.default_duration = if t.timescale > 0 { duration as f64 / t.timescale as f64 } else { 0.0 };
                t.default_size = be32(trex, 16).unwrap_or(0);
                t.default_flags = be32(trex, 20).unwrap_or(0);
            }
        }
    }
    Some(Init { tracks, duration })
}

fn header_duration(b: &[u8]) -> Option<(u32, u64)> {
    match *b.first()? {
        0 => {
            let d = be32(b, 16)?;
            Some((be32(b, 12)?, if d == u32::MAX { 0 } else { d as u64 }))
        }
        1 => {
            let d = be64(b, 24)?;
            Some((be32(b, 20)?, if d == u64::MAX { 0 } else { d }))
        }
        _ => None,
    }
}

fn mp4_track(trak: &[u8], movie_scale: u32) -> Option<Track> {
    let tkhd = child(trak, b"tkhd")?;
    let id = if *tkhd.first()? == 1 { be32(tkhd, 20)? } else { be32(tkhd, 12)? } as u64;
    let mdia = child(trak, b"mdia")?;
    let (timescale, _) = header_duration(child(mdia, b"mdhd")?)?;
    let handler = child(mdia, b"hdlr").and_then(|h| h.get(8..12)).unwrap_or(b"    ");
    let kind = match handler {
        b"vide" => Kind::Video,
        b"soun" => Kind::Audio,
        _ => Kind::Other,
    };
    let stsd = path(mdia, &[b"minf", b"stbl", b"stsd"])?;
    let entry = stsd.get(8..)?;
    let (mut fourcc, _, size) = box_header(entry, 0, true)?;
    let body = entry.get(8..size.min(entry.len()))?;
    let mut config = Vec::new();
    let (mut width, mut height) = (0, 0);
    if kind == Kind::Video {
        width = be16(body, 24).unwrap_or(0) as u32;
        height = be16(body, 26).unwrap_or(0) as u32;
        let kids = body.get(78..).unwrap_or(&[]);
        // an encrypted entry names its format in `sinf/frma`
        if &fourcc == b"encv" {
            if let Some(f) = path(kids, &[b"sinf", b"frma"]).and_then(|f| f.get(..4)) {
                fourcc.copy_from_slice(f);
            }
        }
        for (t, _, b) in boxes(kids) {
            if matches!(&t, b"avcC" | b"av1C" | b"vpcC" | b"hvcC") {
                config = b.to_vec();
                break;
            }
        }
    }
    // the edit list: the first non-empty edit's start is the track's 0; an empty edit before it delays the track
    let mut shift = 0i64;
    if let Some(elst) = path(trak, &[b"edts", b"elst"]) {
        let v1 = elst.first() == Some(&1);
        let n = be32(elst, 4).unwrap_or(0) as usize;
        let entry = if v1 { 20 } else { 12 };
        let mut empty = 0u64;
        for i in 0..n.min(64) {
            let at = 8 + i * entry;
            let (dur, media_time) = if v1 {
                (be64(elst, at).unwrap_or(0), be64(elst, at + 8).unwrap_or(0) as i64)
            } else {
                (be32(elst, at).unwrap_or(0) as u64, be32(elst, at + 4).unwrap_or(0) as i32 as i64)
            };
            if media_time == -1 {
                empty += dur;
                continue;
            }
            let delay = if movie_scale > 0 { (empty as u128 * timescale as u128 / movie_scale as u128) as i64 } else { 0 };
            shift = media_time - delay;
            break;
        }
    }
    Some(Track {
        id,
        kind,
        codec: spec::VideoCodec { fourcc, config },
        width,
        height,
        timescale,
        shift,
        default_duration: 0.0,
        default_size: 0,
        default_flags: 0,
    })
}

/// A progressive MP4's samples, from its sample tables (`stts`, `ctts`, `stss`, `stsz`, `stsc`, `stco`).
fn mp4_table_samples(trak: &[u8], track: &Track, file: &Arc<Vec<u8>>) -> Option<Vec<Sample>> {
    let stbl = path(trak, &[b"mdia", b"minf", b"stbl"])?;
    let sizes: Vec<u32> = if let Some(stsz) = child(stbl, b"stsz") {
        let fixed = be32(stsz, 4)?;
        let n = be32(stsz, 8)? as usize;
        if fixed != 0 { vec![fixed; n.min(1 << 24)] } else { (0..n.min(1 << 24)).map(|i| be32(stsz, 12 + 4 * i)).collect::<Option<_>>()? }
    } else {
        let stz2 = child(stbl, b"stz2")?;
        let field = *stz2.get(7)?;
        let n = be32(stz2, 8)? as usize;
        (0..n.min(1 << 24))
            .map(|i| match field {
                4 => stz2.get(12 + i / 2).map(|b| if i % 2 == 0 { (b >> 4) as u32 } else { (b & 15) as u32 }),
                8 => stz2.get(12 + i).map(|b| *b as u32),
                _ => be16(stz2, 12 + 2 * i).map(u32::from),
            })
            .collect::<Option<_>>()?
    };
    let n = sizes.len();
    let chunks: Vec<u64> = if let Some(stco) = child(stbl, b"stco") {
        (0..be32(stco, 4)? as usize).map(|i| be32(stco, 8 + 4 * i).map(u64::from)).collect::<Option<_>>()?
    } else {
        let co64 = child(stbl, b"co64")?;
        (0..be32(co64, 4)? as usize).map(|i| be64(co64, 8 + 8 * i)).collect::<Option<_>>()?
    };
    let stsc = child(stbl, b"stsc")?;
    let runs: Vec<(u32, u32)> = (0..be32(stsc, 4)? as usize).map(|i| Some((be32(stsc, 8 + 12 * i)?, be32(stsc, 12 + 12 * i)?))).collect::<Option<_>>()?;
    // decode times
    let stts = child(stbl, b"stts")?;
    let mut deltas = Vec::with_capacity(n);
    for i in 0..be32(stts, 4)? as usize {
        let (count, delta) = (be32(stts, 8 + 8 * i)?, be32(stts, 12 + 8 * i)?);
        for _ in 0..count.min((n - deltas.len().min(n)) as u32) {
            deltas.push(delta);
        }
    }
    let mut offsets = vec![0i64; n];
    if let Some(ctts) = child(stbl, b"ctts") {
        let v1 = ctts.first() == Some(&1);
        let mut s = 0usize;
        for i in 0..be32(ctts, 4)? as usize {
            let count = be32(ctts, 8 + 8 * i)? as usize;
            let raw = be32(ctts, 12 + 8 * i)?;
            let off = if v1 { raw as i32 as i64 } else { raw as i64 };
            for _ in 0..count {
                if s < n {
                    offsets[s] = off;
                    s += 1;
                }
            }
        }
    }
    let sync: Option<Vec<u32>> = child(stbl, b"stss").and_then(|s| (0..be32(s, 4)? as usize).map(|i| be32(s, 8 + 4 * i)).collect());
    let video = track.kind == Kind::Video;
    let mut out = Vec::with_capacity(n);
    let mut s = 0usize;
    let mut dts = 0i64;
    for (ci, &chunk) in chunks.iter().enumerate() {
        let per = runs.iter().rev().find(|(first, _)| *first as usize <= ci + 1).map_or(0, |r| r.1) as usize;
        let mut at = chunk as usize;
        for _ in 0..per {
            if s >= n {
                break;
            }
            let size = sizes[s] as usize;
            let delta = *deltas.get(s).unwrap_or(&0) as i64;
            let key = sync.as_ref().map_or(true, |list| list.binary_search(&(s as u32 + 1)).is_ok());
            let data = (video && at.checked_add(size).is_some_and(|end| end <= file.len())).then(|| Bytes { buf: file.clone(), start: at, len: size });
            out.push(Sample {
                track: track.id,
                dts: secs(dts - track.shift, track.timescale),
                pts: secs(dts + offsets[s] - track.shift, track.timescale),
                duration: secs(delta, track.timescale),
                key,
                data,
            });
            dts += delta;
            at += size;
            s += 1;
        }
    }
    Some(out)
}

/// A `moof`'s samples; `seg` is the bytes from the `moof` through its `mdat` (data offsets count from the `moof`).
fn mp4_fragment(seg: &Arc<Vec<u8>>, moof_len: usize, init: &Init) -> Result<Vec<Sample>, ParseError> {
    let moof = seg.get(8..moof_len).ok_or(ParseError)?;
    let mut out = Vec::new();
    let mut next_base: Option<u64> = None;
    for (t, _, traf) in boxes(moof) {
        if &t != b"traf" {
            continue;
        }
        let tfhd = child(traf, b"tfhd").ok_or(ParseError)?;
        let flags = be32(tfhd, 0).ok_or(ParseError)? & 0xFF_FFFF;
        let id = be32(tfhd, 4).ok_or(ParseError)? as u64;
        let Some(track) = init.tracks.iter().find(|t| t.id == id) else { continue };
        let mut at = 8;
        let base_offset = if flags & 1 != 0 {
            let v = be64(tfhd, at);
            at += 8;
            v
        } else {
            None
        };
        let mut take32 = |cond: bool| -> Option<u32> {
            if !cond {
                return None;
            }
            let v = be32(tfhd, at);
            at += 4;
            v
        };
        let _desc = take32(flags & 2 != 0);
        let default_duration = take32(flags & 8 != 0).unwrap_or((track.default_duration * track.timescale as f64).round() as u32);
        let default_size = take32(flags & 0x10 != 0).unwrap_or(track.default_size);
        let default_flags = take32(flags & 0x20 != 0).unwrap_or(track.default_flags);
        // the data's base: the explicit offset, else the moof (default-base-is-moof, and the first traf), else where the
        // previous traf's data ended
        let mut base = base_offset.unwrap_or(if flags & 0x2_0000 != 0 { 0 } else { next_base.unwrap_or(0) });
        let mut dts = child(traf, b"tfdt")
            .and_then(|t| if t.first() == Some(&1) { be64(t, 4) } else { be32(t, 4).map(u64::from) })
            .unwrap_or(0) as i64;
        let video = track.kind == Kind::Video;
        for (t, _, trun) in boxes(traf) {
            if &t != b"trun" {
                continue;
            }
            let v1 = trun.first() == Some(&1);
            let tf = be32(trun, 0).ok_or(ParseError)? & 0xFF_FFFF;
            let count = be32(trun, 4).ok_or(ParseError)? as usize;
            let mut at = 8;
            let mut data_at = base;
            if tf & 1 != 0 {
                data_at = (base as i64 + be32(trun, at).ok_or(ParseError)? as i32 as i64) as u64;
                at += 4;
            }
            let first_flags = if tf & 4 != 0 {
                let f = be32(trun, at).ok_or(ParseError)?;
                at += 4;
                Some(f)
            } else {
                None
            };
            for i in 0..count.min(1 << 20) {
                let mut field = |bit: u32| -> Result<Option<u32>, ParseError> {
                    if tf & bit == 0 {
                        return Ok(None);
                    }
                    let v = be32(trun, at).ok_or(ParseError)?;
                    at += 4;
                    Ok(Some(v))
                };
                let duration = field(0x100)?.unwrap_or(default_duration);
                let size = field(0x200)?.unwrap_or(default_size) as usize;
                let sflags = field(0x400)?.or(if i == 0 { first_flags } else { None }).unwrap_or(default_flags);
                let cto = field(0x800)?.map_or(0, |c| if v1 { c as i32 as i64 } else { c as i64 });
                let key = !video || sflags & 0x1_0000 == 0;
                let start = data_at as usize;
                let data = if video {
                    if start.checked_add(size).is_none_or(|end| end > seg.len()) {
                        return Err(ParseError);
                    }
                    Some(Bytes { buf: seg.clone(), start, len: size })
                } else {
                    None
                };
                out.push(Sample {
                    track: id,
                    dts: secs(dts - track.shift, track.timescale),
                    pts: secs(dts + cto - track.shift, track.timescale),
                    duration: secs(duration as i64, track.timescale),
                    key,
                    data,
                });
                dts += duration as i64;
                data_at += size as u64;
            }
            base = data_at;
        }
        next_base = Some(base);
    }
    Ok(out)
}

// ---- WebM -----------------------------------------------------------------------------------------

fn ebml_id(b: &[u8], at: usize) -> Option<(u32, usize)> {
    let first = *b.get(at)?;
    let len = first.leading_zeros() as usize + 1;
    if len > 4 {
        return Some((0, 1));
    }
    let mut v = 0u32;
    for i in 0..len {
        v = (v << 8) | *b.get(at + i)? as u32;
    }
    Some((v, len))
}

/// An EBML variable-size integer: (value, length); `None` value for the reserved "unknown" size.
fn ebml_size(b: &[u8], at: usize) -> Option<(Option<u64>, usize)> {
    let first = *b.get(at)?;
    let len = first.leading_zeros() as usize + 1;
    if len > 8 {
        return Some((Some(0), 1));
    }
    let mut v = (first as u64) & (0xFF >> len);
    let mut all_ones = v == (0xFF >> len) as u64;
    for i in 1..len {
        let byte = *b.get(at + i)?;
        all_ones &= byte == 0xFF;
        v = (v << 8) | byte as u64;
    }
    Some((if all_ones { None } else { Some(v) }, len))
}

fn elements(b: &[u8]) -> impl Iterator<Item = (u32, &[u8])> {
    let mut at = 0usize;
    std::iter::from_fn(move || {
        let (id, il) = ebml_id(b, at)?;
        let (size, sl) = ebml_size(b, at + il)?;
        let start = at + il + sl;
        let end = match size {
            Some(s) => (start as u64).checked_add(s)?.min(b.len() as u64) as usize,
            None => b.len(),
        };
        let body = b.get(start..end)?;
        at = end;
        Some((id, body))
    })
}

fn uint(b: &[u8]) -> u64 {
    b.iter().take(8).fold(0, |v, byte| (v << 8) | *byte as u64)
}

fn float(b: &[u8]) -> Option<f64> {
    match b.len() {
        4 => Some(f32::from_be_bytes(b.try_into().ok()?) as f64),
        8 => Some(f64::from_be_bytes(b.try_into().ok()?)),
        _ => None,
    }
}

fn webm_tracks(tracks: &[u8]) -> Vec<Track> {
    let mut out = Vec::new();
    for (_, entry) in elements(tracks).filter(|(id, _)| *id == 0xAE) {
        let field = |want: u32| elements(entry).find(|(id, _)| *id == want).map(|(_, v)| v);
        let id = field(0xD7).map(uint).unwrap_or(0);
        let kind = match field(0x83).map(uint) {
            Some(1) => Kind::Video,
            Some(2) => Kind::Audio,
            _ => Kind::Other,
        };
        let codec_id = field(0x86).map(|v| String::from_utf8_lossy(v).trim_end_matches('\0').to_string()).unwrap_or_default();
        let fourcc = match codec_id.as_str() {
            "V_VP8" => *b"vp08",
            "V_VP9" => *b"vp09",
            "V_AV1" => *b"av01",
            "V_MPEG4/ISO/AVC" => *b"avc1",
            "A_OPUS" => *b"Opus",
            "A_VORBIS" => *b"vorb",
            "A_AAC" => *b"mp4a",
            _ => *b"    ",
        };
        let config = field(0x63A2).map(<[u8]>::to_vec).unwrap_or_default();
        let (mut width, mut height) = (0, 0);
        if let Some(video) = field(0xE0) {
            let f = |want: u32| elements(video).find(|(id, _)| *id == want).map(|(_, v)| uint(v) as u32);
            width = f(0xB0).unwrap_or(0);
            height = f(0xBA).unwrap_or(0);
        }
        let default_duration = field(0x23E383).map(|v| uint(v) as f64 / 1e9).unwrap_or(0.0);
        out.push(Track {
            id,
            kind,
            codec: spec::VideoCodec { fourcc, config },
            width,
            height,
            timescale: 0,
            shift: 0,
            default_duration,
            default_size: 0,
            default_flags: 0,
        });
    }
    out
}

// ---- the parser -----------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Format {
    Mp4,
    WebM,
}

/// A byte stream's parser: MSE's segment parser loop, over the bytes appended so far.
pub struct Parser {
    format: Option<Format>,
    pending: Vec<u8>,
    init: Option<Init>,
    /// WebM: the segment's `TimecodeScale` (ns per tick), its stated duration, the current cluster's time.
    timecode_scale: u64,
    webm_duration: Option<f64>,
    cluster: Option<u64>,
    /// WebM: each track's last frame, held until the next one gives its duration.
    held: Vec<Sample>,
}

impl Default for Parser {
    fn default() -> Self {
        Parser { format: None, pending: Vec::new(), init: None, timecode_scale: 1_000_000, webm_duration: None, cluster: None, held: Vec::new() }
    }
}

impl Parser {
    /// A parser for a `SourceBuffer` of MIME type `mime` (`video/mp4`, `audio/webm`, ...); `None` for other formats.
    pub fn for_mime(mime: &str) -> Option<Parser> {
        let base = mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        let format = match base.as_str() {
            "video/mp4" | "audio/mp4" => Format::Mp4,
            "video/webm" | "audio/webm" => Format::WebM,
            _ => return None,
        };
        Some(Parser { format: Some(format), ..Parser::default() })
    }

    /// Forget a partly-read segment (`abort()`, `changeType()`): the next bytes start a new one. The init segment
    /// stays.
    pub fn reset(&mut self) {
        self.pending.clear();
        self.held.clear();
        self.cluster = None;
    }

    /// Switch the byte stream format (`changeType()`).
    pub fn change_type(&mut self, mime: &str) {
        if let Some(p) = Parser::for_mime(mime) {
            self.format = p.format;
        }
        self.reset();
    }

    /// Append `bytes`; what they completed.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Event>, ParseError> {
        self.pending.extend_from_slice(bytes);
        let format = match self.format {
            Some(f) => f,
            None => {
                let f = if self.pending.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) { Format::WebM } else { Format::Mp4 };
                self.format = Some(f);
                f
            }
        };
        let mut events = Vec::new();
        match format {
            Format::Mp4 => self.push_mp4(&mut events)?,
            Format::WebM => self.push_webm(&mut events)?,
        }
        Ok(events)
    }

    fn push_mp4(&mut self, events: &mut Vec<Event>) -> Result<(), ParseError> {
        let mut at = 0usize;
        loop {
            let Some((ty, _, size)) = box_header(&self.pending, at, false) else { break };
            if !ty.iter().all(|c| c.is_ascii_graphic() || *c == b' ') {
                return Err(ParseError);
            }
            if &ty == b"moof" {
                // the fragment's samples are in the `mdat` after it: wait for that box, whatever comes between
                let mut end = at + size;
                let mut mdat_end = None;
                while let Some((t, _, s)) = box_header(&self.pending, end, false) {
                    if end + s > self.pending.len() {
                        break;
                    }
                    end += s;
                    if &t == b"mdat" {
                        mdat_end = Some(end);
                        break;
                    }
                }
                let Some(mdat_end) = mdat_end else { break };
                let Some(init) = &self.init else { return Err(ParseError) };
                let seg = Arc::new(self.pending[at..mdat_end].to_vec());
                let samples = mp4_fragment(&seg, size, init)?;
                if !samples.is_empty() {
                    events.push(Event::Samples(samples));
                }
                at = mdat_end;
                continue;
            }
            if at + size > self.pending.len() {
                break;
            }
            if &ty == b"moov" {
                let init = mp4_init(&self.pending[at + 8..at + size]).ok_or(ParseError)?;
                self.init = Some(init.clone());
                events.push(Event::Init(init));
            }
            at += size;
        }
        self.pending.drain(..at);
        Ok(())
    }

    fn push_webm(&mut self, events: &mut Vec<Event>) -> Result<(), ParseError> {
        let mut at = 0usize;
        let mut samples = Vec::new();
        loop {
            let Some((id, il)) = ebml_id(&self.pending, at) else { break };
            let Some((size, sl)) = ebml_size(&self.pending, at + il) else { break };
            let header = il + sl;
            match id {
                // master elements entered without waiting for their ends: the segment, a cluster, a block group's
                // parent levels
                0x1853_8067 => {
                    at += header;
                    continue;
                }
                0x1F43_B675 => {
                    self.cluster = None;
                    at += header;
                    continue;
                }
                0 => return Err(ParseError),
                _ => {}
            }
            let Some(size) = size else {
                // an unknown-size element that is not a master this parser enters
                at += header;
                continue;
            };
            let end = at + header + size as usize;
            if end > self.pending.len() {
                break;
            }
            let body = &self.pending[at + header..end];
            match id {
                0x1A45_DFA3 => {
                    let doctype = elements(body).find(|(id, _)| *id == 0x4282).map(|(_, v)| v);
                    if doctype.is_some_and(|d| d != b"webm" && d != b"matroska") {
                        return Err(ParseError);
                    }
                }
                0x1549_A966 => {
                    for (id, v) in elements(body) {
                        match id {
                            0x2AD7B1 => self.timecode_scale = uint(v).max(1),
                            0x4489 => self.webm_duration = float(v),
                            _ => {}
                        }
                    }
                }
                0x1654_AE6B => {
                    let duration = self.webm_duration.filter(|d| d.is_finite() && *d > 0.0).map(|d| d * self.timecode_scale as f64 / 1e9);
                    let init = Init { tracks: webm_tracks(body), duration };
                    self.init = Some(init.clone());
                    events.push(Event::Init(init));
                }
                0xE7 => self.cluster = Some(uint(body)),
                0xA3 => {
                    if let Some(s) = self.webm_block(body, true, None)? {
                        samples.push(s);
                    }
                }
                0xA0 => {
                    let block = elements(body).find(|(id, _)| *id == 0xA1).map(|(_, v)| v);
                    let duration = elements(body).find(|(id, _)| *id == 0x9B).map(|(_, v)| uint(v));
                    let referenced = elements(body).any(|(id, _)| id == 0xFB);
                    if let Some(block) = block {
                        if let Some(mut s) = self.webm_block(block, !referenced, duration)? {
                            s.key = !referenced;
                            samples.push(s);
                        }
                    }
                }
                _ => {}
            }
            at = end;
        }
        self.pending.drain(..at);
        // each track's frames take their durations from the frame after; the last stays held for the next append,
        // unless its duration was stated
        let mut done = Vec::new();
        for s in samples {
            if let Some(h) = self.held.iter().position(|h| h.track == s.track) {
                let mut prev = self.held.remove(h);
                if prev.duration <= 0.0 {
                    prev.duration = (s.pts - prev.pts).max(0.0);
                }
                done.push(prev);
            }
            if s.duration > 0.0 {
                done.push(s);
            } else {
                self.held.push(s);
            }
        }
        // a held frame whose track gives a default duration need not wait
        let init = self.init.as_ref();
        self.held.retain_mut(|h| {
            let default = init.and_then(|i| i.tracks.iter().find(|t| t.id == h.track)).map_or(0.0, |t| t.default_duration);
            if default > 0.0 {
                h.duration = default;
                done.push(h.clone());
                false
            } else {
                true
            }
        });
        if !done.is_empty() {
            events.push(Event::Samples(done));
        }
        Ok(())
    }

    /// A WebM block's frame. `duration` is a block group's `BlockDuration`, in ticks.
    fn webm_block(&self, b: &[u8], simple: bool, duration: Option<u64>) -> Result<Option<Sample>, ParseError> {
        let (track, tl) = ebml_size(b, 0).ok_or(ParseError)?;
        let track = track.ok_or(ParseError)?;
        let rel = be16(b, tl).ok_or(ParseError)? as i16 as i64;
        let flags = *b.get(tl + 2).ok_or(ParseError)?;
        let Some(init) = &self.init else { return Err(ParseError) };
        let Some(t) = init.tracks.iter().find(|t| t.id == track) else { return Ok(None) };
        let cluster = self.cluster.ok_or(ParseError)? as i64;
        let scale = self.timecode_scale as f64 / 1e9;
        let pts = (cluster + rel) as f64 * scale;
        let lacing = (flags >> 1) & 3;
        let video = t.kind == Kind::Video;
        let data = (video && lacing == 0).then(|| {
            let start = tl + 3;
            Bytes { buf: Arc::new(b[start..].to_vec()), start: 0, len: b.len() - start }
        });
        let frames = if lacing == 0 { 1 } else { *b.get(tl + 3).unwrap_or(&0) as u64 + 1 };
        let duration = match duration {
            Some(d) => d as f64 * scale,
            None if t.default_duration > 0.0 => t.default_duration * frames as f64,
            None => 0.0,
        };
        Ok(Some(Sample { track, dts: pts, pts, duration, key: !simple || flags & 0x80 != 0, data }))
    }
}

/// A whole file's tracks and samples: a progressive MP4's sample tables, or the fragments and clusters of a
/// fragmented MP4 or a WebM read as one append. `None` when it is neither container.
pub fn movie(file: Arc<Vec<u8>>) -> Option<(Init, Vec<Sample>)> {
    if file.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return whole_stream(&file, Format::WebM);
    }
    let moov = boxes(&file).take(4096).find(|(t, _, _)| t == b"moov").map(|(_, at, body)| (at, body.len()))?;
    let moov = &file[moov.0..moov.0 + moov.1];
    if child(moov, b"mvex").is_some() {
        return whole_stream(&file, Format::Mp4);
    }
    let init = mp4_init(moov)?;
    let mvhd_scale = header_duration(child(moov, b"mvhd")?)?.0;
    let mut samples = Vec::new();
    for (_, _, trak) in boxes(moov).filter(|(t, _, _)| t == b"trak") {
        let Some(track) = mp4_track(trak, mvhd_scale) else { continue };
        if let Some(s) = mp4_table_samples(trak, &track, &file) {
            samples.extend(s);
        }
    }
    Some((init, samples))
}

fn whole_stream(file: &[u8], format: Format) -> Option<(Init, Vec<Sample>)> {
    let mut p = Parser { format: Some(format), ..Parser::default() };
    let mut samples = Vec::new();
    for e in p.push(file).ok()? {
        if let Event::Samples(s) = e {
            samples.extend(s);
        }
    }
    samples.extend(p.held.drain(..));
    Some((p.init?, samples))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wpt(path: &str) -> Arc<Vec<u8>> {
        Arc::new(std::fs::read(format!("{}/../../tools/realweb/seed/media/{path}", env!("CARGO_MANIFEST_DIR"))).unwrap())
    }

    fn video(samples: &[Sample], init: &Init) -> Vec<Sample> {
        let id = init.tracks.iter().find(|t| t.kind == Kind::Video).unwrap().id;
        samples.iter().filter(|s| s.track == id).cloned().collect()
    }

    #[test]
    fn a_progressive_mp4_gives_its_samples_and_times() {
        // webcodecs/h264.mp4: ten 320x240 frames at 10 fps, one keyframe
        let (init, samples) = movie(wpt("vectors/h264.mp4")).unwrap();
        let v = video(&samples, &init);
        let track = init.tracks.iter().find(|t| t.kind == Kind::Video).unwrap();
        assert_eq!(&track.codec.fourcc, b"avc1");
        assert_eq!((track.width, track.height), (320, 240));
        assert_eq!(v.len(), 10);
        assert!(v[0].key && v.iter().skip(1).all(|s| !s.key));
        let mut pts: Vec<f64> = v.iter().map(|s| s.pts).collect();
        pts.sort_by(f64::total_cmp);
        assert!((pts[0]).abs() < 1e-6, "{pts:?}");
        assert!((pts[1] - 0.1).abs() < 1e-6, "{pts:?}");
        assert!(v.iter().all(|s| s.data.is_some()));
    }

    #[test]
    fn a_fragmented_mp4_appended_in_pieces_gives_the_same_samples() {
        let file = wpt("vectors/test-v-128k-320x240-24fps-8kfr.mp4");
        let (init, whole) = movie(file.clone()).unwrap();
        assert!(init.duration.is_some());
        let mut p = Parser::for_mime("video/mp4; codecs=\"avc1.4d401e\"").unwrap();
        let mut pieces = Vec::new();
        let mut inits = 0;
        for chunk in file.chunks(997) {
            for e in p.push(chunk).unwrap() {
                match e {
                    Event::Init(_) => inits += 1,
                    Event::Samples(s) => pieces.extend(s),
                }
            }
        }
        assert_eq!(inits, 1);
        assert_eq!(pieces.len(), whole.len());
        assert!(pieces.len() >= 40, "{}", pieces.len());
        let last = pieces.iter().map(|s| s.pts + s.duration).fold(0.0, f64::max);
        // 24 fps for about two seconds
        assert!(last > 1.5 && last < 4.0, "{last}");
        assert!(pieces[0].key && pieces[0].data.as_ref().is_some_and(|d| d.as_slice().len() > 4));
    }

    #[test]
    fn a_webm_gives_its_blocks() {
        let (init, samples) = movie(wpt("vectors/test-av-384k-44100Hz-1ch-320x240-30fps-10kfr.webm")).unwrap();
        let track = init.tracks.iter().find(|t| t.kind == Kind::Video).unwrap();
        assert_eq!((track.width, track.height), (320, 240));
        let v = video(&samples, &init);
        assert!(v.len() > 30, "{}", v.len());
        assert!(v[0].key);
        assert!(v.iter().all(|s| s.duration > 0.0));
        assert!(init.tracks.iter().any(|t| t.kind == Kind::Audio));
        assert!(samples.iter().any(|s| s.data.is_none()));
    }

    #[test]
    fn a_media_segment_before_any_init_segment_is_an_error() {
        let file = wpt("vectors/test-v-128k-320x240-24fps-8kfr.mp4");
        let moof = boxes(&file).find(|(t, _, _)| t == b"moof").map(|(_, at, _)| at - 8).unwrap();
        let mut p = Parser::for_mime("video/mp4").unwrap();
        assert_eq!(p.push(&file[moof..]).unwrap_err(), ParseError);
    }
}
