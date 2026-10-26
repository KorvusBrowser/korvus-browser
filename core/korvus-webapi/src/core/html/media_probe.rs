//! `core/html/media_probe.rs` -- what a media resource's HEADER says.
//!
//! Nothing here decodes a sample. A media element needs three facts from its
//! resource before it can report `HAVE_METADATA` -- whether the container is one the browser opens at
//! all, how long the resource is, and (for video) how large a frame is -- and all three are written in
//! the container's header. This file reads them and nothing else.
//!
//! THE DURATION IS THE ONE CHROME COMPUTES, which is not always the one the container states. Chrome's
//! demuxer (FFmpeg) takes the longest STREAM, each stream's length after its edit list, rescaled to
//! the stream's own clock and rounded to the microsecond. Measured on Chrome 150 with WPT's
//! `2x2-green.mp4` (fixture `M-media-play`): `duration` is `0.156009`. The movie header says 0.182 s;
//! the audio track's edit list keeps 156 ms of it, which at 44.1 kHz is 6880 samples (6879.6 rounded),
//! and 6880 / 44100 s is 156009 us. Reading `mvhd` would answer 0.182.
//!
//! A CONTAINER THAT STATES NO LENGTH (a live WebM, an Ogg stream cut short) answers `Infinity`, which
//! is what Chrome reports for a resource of unknown duration. A byte string no container claims is
//! not media: the element fails with MEDIA_ERR_SRC_NOT_SUPPORTED, as Chrome's did for the fixture's
//! `bad.xyz`.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

/// The three header facts a media element reports once it has metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaProbe {
    /// Seconds, microsecond-rounded as Chrome's pipeline keeps it; `f64::INFINITY` when unstated.
    pub duration: f64,
    /// The video track's presentation size; `(0, 0)` for audio.
    pub width: u32,
    pub height: u32,
}

/// Read `bytes` as a media resource: `None` when no supported container claims it.
pub fn probe(bytes: &[u8]) -> Option<MediaProbe> {
    mp4(bytes).or_else(|| webm(bytes)).or_else(|| ogg(bytes)).or_else(|| wav(bytes)).or_else(|| flac(bytes)).or_else(|| mp3(bytes)).or_else(|| adts(bytes))
}

/// The probes of the media bodies this page fetched, by request URL. The kernel's subresource loader
/// holds the bytes when a `video`/`audio` response arrives and writes the probe here; the element whose
/// fetch it was reads it. The bytes themselves are never handed to the page's realm.
#[derive(Clone, Default)]
pub struct MediaProbes(Arc<Mutex<ProbeList>>);

#[derive(Default)]
struct ProbeList {
    map: HashMap<String, Option<MediaProbe>>,
    order: VecDeque<String>,
    /// A video's body, kept for the element whose fetch it was to decode (`video_pipe.rs`): taken once.
    bodies: HashMap<String, Arc<Vec<u8>>>,
}

impl MediaProbes {
    /// How many answers are kept. An answer is read once, moments after it is written.
    const KEEP: usize = 256;

    pub fn insert(&self, url: &str, probe: Option<MediaProbe>) {
        let Ok(mut l) = self.0.lock() else { return };
        if l.map.insert(url.to_string(), probe).is_none() {
            l.order.push_back(url.to_string());
        }
        while l.order.len() > Self::KEEP {
            if let Some(k) = l.order.pop_front() {
                l.map.remove(&k);
            }
        }
    }

    /// `None`: no answer was recorded; `Some(None)`: the body is not media.
    pub fn get(&self, url: &str) -> Option<Option<MediaProbe>> {
        self.0.lock().ok()?.map.get(url).cloned()
    }

    /// The largest video body kept for decoding; a larger one is shown by its poster, as before.
    const KEEP_BODY: usize = 512 << 20;

    /// Keep a `video` response's body for its element to decode, when its header found a picture to decode.
    pub fn keep_body(&self, url: &str, probe: Option<&MediaProbe>, bytes: &[u8]) {
        if probe.is_none_or(|p| p.width == 0) || bytes.len() > Self::KEEP_BODY {
            return;
        }
        let Ok(mut l) = self.0.lock() else { return };
        l.bodies.insert(url.to_string(), Arc::new(bytes.to_vec()));
        // a body nobody took (its element was removed, or loaded something else) goes with its probe's turn
        let ProbeList { map, bodies, .. } = &mut *l;
        bodies.retain(|k, _| map.contains_key(k));
    }

    /// The body kept for `url`, handed over once.
    pub fn take_body(&self, url: &str) -> Option<Arc<Vec<u8>>> {
        self.0.lock().ok()?.bodies.remove(url)
    }
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn be64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
}
fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn le64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// `n / d` seconds rounded to the microsecond, as FFmpeg's `av_rescale_q` into Chrome's time base.
fn micros(n: u128, d: u128) -> Option<f64> {
    (d > 0).then(|| ((n * 1_000_000 + d / 2) / d) as f64 / 1e6)
}

// ---- ISO BMFF (MP4, MOV, M4A) -------------------------------------------------------------------

/// The boxes directly inside `b`, as (type, body). A box running past the end is cut at the end.
fn boxes(b: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    let mut at = 0usize;
    std::iter::from_fn(move || {
        let size = be32(b, at)? as u64;
        let ty: [u8; 4] = b.get(at + 4..at + 8)?.try_into().ok()?;
        let (header, size) = match size {
            1 => (16u64, be64(b, at + 8)?),
            0 => (8, (b.len() - at) as u64),
            n => (8, n),
        };
        if size < header {
            return None;
        }
        let end = (at as u64).checked_add(size)?.min(b.len() as u64) as usize;
        let body = b.get(at + header as usize..end)?;
        at = end;
        Some((ty, body))
    })
}

fn child<'a>(b: &'a [u8], ty: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(b).find(|(t, _)| t == ty).map(|(_, body)| body)
}

/// A box type is four printable characters; random bytes rarely are, which keeps a text file or an
/// image from being read as a movie.
fn box_type(t: &[u8; 4]) -> bool {
    t.iter().all(|c| c.is_ascii_alphanumeric() || *c == b' ' || *c == b'-')
}

fn mp4(b: &[u8]) -> Option<MediaProbe> {
    let (first, _) = boxes(b).next()?;
    if !box_type(&first) || !matches!(&first, b"ftyp" | b"styp" | b"moov" | b"mdat" | b"free" | b"skip" | b"wide" | b"pdin" | b"sidx") {
        return None;
    }
    let moov = boxes(b).take(1024).find(|(t, _)| t == b"moov").map(|(_, body)| body)?;
    let mvhd = child(moov, b"mvhd")?;
    let (movie_scale, movie_duration) = header_duration(mvhd)?;
    let mut longest: Option<f64> = None;
    let (mut width, mut height) = (0, 0);
    for (_, trak) in boxes(moov).filter(|(t, _)| t == b"trak") {
        let Some(mdia) = child(trak, b"mdia") else { continue };
        let Some((scale, media_duration)) = child(mdia, b"mdhd").and_then(header_duration) else { continue };
        let handler = child(mdia, b"hdlr").and_then(|h| h.get(8..12));
        // The edit list decides how much of the track is presented; each segment is in MOVIE time.
        let edits = child(trak, b"edts").and_then(|e| child(e, b"elst")).and_then(edit_segments);
        let track = match edits {
            Some(segments) if movie_scale > 0 => segments
                .iter()
                .map(|s| (*s as u128 * scale as u128 + movie_scale as u128 / 2) / movie_scale as u128)
                .sum::<u128>(),
            _ => media_duration as u128,
        };
        if let Some(d) = micros(track, scale as u128) {
            longest = Some(longest.map_or(d, |l: f64| l.max(d)));
        }
        if handler == Some(b"vide".as_slice()) && width == 0 {
            if let Some((w, h)) = child(trak, b"tkhd").and_then(track_size) {
                (width, height) = (w, h);
            }
        }
    }
    let duration = match longest {
        Some(d) if d > 0.0 => d,
        _ => micros(movie_duration as u128, movie_scale as u128).filter(|d| *d > 0.0).unwrap_or(f64::INFINITY),
    };
    Some(MediaProbe { duration, width, height })
}

/// `(timescale, duration)` of an `mvhd` or `mdhd` body.
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

/// The segment durations of an `elst` body, in movie time.
fn edit_segments(b: &[u8]) -> Option<Vec<u64>> {
    let version = *b.first()?;
    let count = be32(b, 4)? as usize;
    let entry = if version == 1 { 20 } else { 12 };
    (0..count.min(4096))
        .map(|i| {
            let at = 8 + i * entry;
            if version == 1 { be64(b, at) } else { be32(b, at).map(u64::from) }
        })
        .collect()
}

/// A `tkhd` body's presentation width and height (16.16 fixed point).
fn track_size(b: &[u8]) -> Option<(u32, u32)> {
    let at = if *b.first()? == 1 { 88 } else { 76 };
    Some((be32(b, at)? >> 16, be32(b, at + 4)? >> 16))
}

// ---- Matroska / WebM ------------------------------------------------------------------------------

/// An EBML element ID, marker bits kept: (id, length in bytes).
fn ebml_id(b: &[u8], at: usize) -> Option<(u32, usize)> {
    let first = *b.get(at)?;
    let len = first.leading_zeros() as usize + 1;
    if len > 4 {
        return None;
    }
    let mut v = 0u32;
    for i in 0..len {
        v = (v << 8) | *b.get(at + i)? as u32;
    }
    Some((v, len))
}

/// An EBML size: (value, length in bytes); `None` value for the reserved "unknown" size.
fn ebml_size(b: &[u8], at: usize) -> Option<(Option<u64>, usize)> {
    let first = *b.get(at)?;
    let len = first.leading_zeros() as usize + 1;
    if len > 8 {
        return None;
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

/// The elements directly inside `b`, as (id, body). An unknown size runs to the end of `b`.
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

fn webm(b: &[u8]) -> Option<MediaProbe> {
    if b.get(..4)? != [0x1A, 0x45, 0xDF, 0xA3] {
        return None;
    }
    let mut top = elements(b);
    let (_, header) = top.next()?;
    let doctype = elements(header).find(|(id, _)| *id == 0x4282).map(|(_, v)| v)?;
    if doctype != b"webm" && doctype != b"matroska" {
        return None;
    }
    let (_, segment) = top.find(|(id, _)| *id == 0x1853_8067)?;
    let mut scale = 1_000_000u64;
    let mut duration: Option<f64> = None;
    let (mut width, mut height) = (0, 0);
    for (id, body) in elements(segment) {
        match id {
            0x1549_A966 => {
                for (id, v) in elements(body) {
                    match id {
                        0x2AD7B1 => scale = uint(v).max(1),
                        0x4489 => {
                            duration = match v.len() {
                                4 => Some(f32::from_be_bytes(v.try_into().ok()?) as f64),
                                8 => Some(f64::from_be_bytes(v.try_into().ok()?)),
                                _ => None,
                            }
                        }
                        _ => {}
                    }
                }
            }
            0x1654_AE6B => {
                for (_, entry) in elements(body).filter(|(id, _)| *id == 0xAE) {
                    let Some((_, video)) = elements(entry).find(|(id, _)| *id == 0xE0) else { continue };
                    let field = |want: u32| elements(video).find(|(id, _)| *id == want).map(|(_, v)| uint(v) as u32);
                    let w = field(0x54B0).or_else(|| field(0xB0)).unwrap_or(0);
                    let h = field(0x54BA).or_else(|| field(0xBA)).unwrap_or(0);
                    if width == 0 && w > 0 {
                        (width, height) = (w, h);
                    }
                }
            }
            // The first cluster is media data: every header element comes before it.
            0x1F43_B675 => break,
            _ => {}
        }
    }
    let duration = match duration {
        Some(d) if d.is_finite() && d > 0.0 => ((d * scale as f64 / 1000.0).round()) / 1e6,
        _ => f64::INFINITY,
    };
    Some(MediaProbe { duration, width, height })
}

// ---- Ogg (Vorbis, Opus) -----------------------------------------------------------------------

fn ogg(b: &[u8]) -> Option<MediaProbe> {
    if b.get(..4)? != b"OggS" {
        return None;
    }
    // The first page's first packet names the codec. Chrome dropped Theora, so a Theora stream is not
    // media it opens.
    let segments = *b.get(26)? as usize;
    let packet = b.get(27 + segments..)?;
    let (rate, pre_skip) = if packet.starts_with(b"\x01vorbis") {
        (le32(packet, 12)? as u64, 0u64)
    } else if packet.starts_with(b"OpusHead") {
        (48_000, le16(packet, 10)? as u64)
    } else {
        return None;
    };
    // The last page's granule position is the stream's length in samples.
    let last = (0..b.len().saturating_sub(27)).rev().find(|&i| b[i..].starts_with(b"OggS") && b.get(i + 4) == Some(&0))?;
    let granule = le64(b, last + 6)?;
    let duration = if granule == u64::MAX || rate == 0 {
        f64::INFINITY
    } else {
        micros(granule.saturating_sub(pre_skip) as u128, rate as u128).unwrap_or(f64::INFINITY)
    };
    Some(MediaProbe { duration, width: 0, height: 0 })
}

// ---- WAV, FLAC -----------------------------------------------------------------------------------

fn wav(b: &[u8]) -> Option<MediaProbe> {
    if b.get(..4)? != b"RIFF" || b.get(8..12)? != b"WAVE" {
        return None;
    }
    let (mut byte_rate, mut data) = (None, None);
    let mut at = 12;
    while let Some(size) = le32(b, at + 4) {
        let id = b.get(at..at + 4)?;
        if id == b"fmt " {
            byte_rate = le32(b, at + 16);
        } else if id == b"data" {
            data = Some(size);
            break;
        }
        at = at.checked_add(8 + size as usize + (size as usize & 1))?;
    }
    let duration = micros(data? as u128, byte_rate? as u128)?;
    Some(MediaProbe { duration, width: 0, height: 0 })
}

fn flac(b: &[u8]) -> Option<MediaProbe> {
    if b.get(..4)? != b"fLaC" || b.get(4)? & 0x7F != 0 {
        return None;
    }
    let info = b.get(8..8 + 34)?;
    let rate = ((info[10] as u32) << 12) | ((info[11] as u32) << 4) | ((info[12] as u32) >> 4);
    let total = (((info[13] & 0x0F) as u64) << 32) | be32(info, 14)? as u64;
    let duration = if total == 0 { f64::INFINITY } else { micros(total as u128, rate as u128)? };
    Some(MediaProbe { duration, width: 0, height: 0 })
}

// ---- MPEG audio (MP3), ADTS AAC ----------------------------------------------------------------

/// One MPEG audio frame header at `at`: (frame length in bytes, samples per frame, sample rate,
/// bitrate in kbit/s, MPEG-1?, mono?).
fn mpeg_frame(b: &[u8], at: usize) -> Option<(usize, u32, u32, u32, bool, bool)> {
    let h = b.get(at..at + 4)?;
    if h[0] != 0xFF || h[1] & 0xE0 != 0xE0 {
        return None;
    }
    let version = (h[1] >> 3) & 3; // 3 = MPEG-1, 2 = MPEG-2, 0 = MPEG-2.5
    let layer = (h[1] >> 1) & 3; // 1 = III, 2 = II, 3 = I
    let bitrate_index = (h[2] >> 4) as usize;
    let rate_index = ((h[2] >> 2) & 3) as usize;
    if version == 1 || layer == 0 || bitrate_index == 0 || bitrate_index == 15 || rate_index == 3 {
        return None;
    }
    let mpeg1 = version == 3;
    let kbps: &[u32; 15] = match (mpeg1, layer) {
        (true, 3) => &[0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448],
        (true, 2) => &[0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384],
        (true, _) => &[0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320],
        (false, 3) => &[0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256],
        (false, _) => &[0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
    };
    let rates = match version {
        3 => [44_100, 48_000, 32_000],
        2 => [22_050, 24_000, 16_000],
        _ => [11_025, 12_000, 8_000],
    };
    let (bitrate, rate) = (kbps[bitrate_index], rates[rate_index]);
    let padding = ((h[2] >> 1) & 1) as usize;
    let samples = match (layer, mpeg1) {
        (3, _) => 384,
        (2, _) | (1, true) => 1152,
        _ => 576,
    };
    let len = if layer == 3 {
        (12 * bitrate as usize * 1000 / rate as usize + padding) * 4
    } else {
        samples as usize / 8 * bitrate as usize * 1000 / rate as usize + padding
    };
    Some((len, samples, rate, bitrate, mpeg1, h[3] >> 6 == 3))
}

fn mp3(b: &[u8]) -> Option<MediaProbe> {
    let mut at = 0usize;
    let tagged = b.starts_with(b"ID3");
    if tagged {
        let size = b.get(6..10)?.iter().fold(0usize, |v, x| (v << 7) | (*x & 0x7F) as usize);
        at = 10 + size + if b.get(5)? & 0x10 != 0 { 10 } else { 0 };
    }
    let (len, samples, rate, bitrate, mpeg1, mono) = mpeg_frame(b, at)?;
    // Without a tag, a second frame right after the first is what tells MPEG audio from noise that
    // happens to start with a sync word.
    if !tagged {
        mpeg_frame(b, at + len)?;
    }
    let side = match (mpeg1, mono) {
        (true, true) => 17,
        (true, false) => 32,
        (false, true) => 9,
        (false, false) => 17,
    };
    let xing = at + 4 + side;
    let frames = if matches!(b.get(xing..xing + 4), Some(b"Xing") | Some(b"Info")) && be32(b, xing + 4)? & 1 != 0 {
        be32(b, xing + 8)
    } else if b.get(at + 36..at + 40) == Some(b"VBRI") {
        be32(b, at + 50)
    } else {
        None
    };
    let duration = match frames {
        Some(n) => micros(n as u128 * samples as u128, rate as u128)?,
        None => {
            let tail = if b.len() >= 128 && b[b.len() - 128..].starts_with(b"TAG") { 128 } else { 0 };
            let audio = b.len().saturating_sub(at + tail);
            micros(audio as u128 * 8, bitrate as u128 * 1000)?
        }
    };
    Some(MediaProbe { duration, width: 0, height: 0 })
}

fn adts(b: &[u8]) -> Option<MediaProbe> {
    const RATES: [u32; 13] = [96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000, 7_350];
    let header = |at: usize| -> Option<(usize, u32)> {
        let h = b.get(at..at + 7)?;
        if h[0] != 0xFF || h[1] & 0xF6 != 0xF0 {
            return None;
        }
        let rate = *RATES.get(((h[2] >> 2) & 0x0F) as usize)?;
        let len = (((h[3] & 3) as usize) << 11) | ((h[4] as usize) << 3) | ((h[5] as usize) >> 5);
        (len >= 7).then_some((len, rate))
    };
    let (first, rate) = header(0)?;
    header(first)?;
    let (mut at, mut frames) = (0usize, 0u64);
    while let Some((len, _)) = header(at) {
        frames += 1;
        at += len;
    }
    Some(MediaProbe { duration: micros(frames as u128 * 1024, rate as u128)?, width: 0, height: 0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WPT's `2x2-green.mp4`, as fixture `M-media-play` serves it: Chrome 150 reports 0.156009.
    #[test]
    fn the_fixture_mp4_reads_the_duration_chrome_reports() {
        let bytes = include_bytes!("../../../../../tools/realweb/seed/media/v.mp4");
        let p = probe(bytes).expect("an mp4");
        assert_eq!(p.duration, 0.156009);
        assert_eq!((p.width, p.height), (2, 2));
    }

    #[test]
    fn a_byte_string_no_container_claims_is_not_media() {
        assert_eq!(probe(b"not media"), None);
        assert_eq!(probe(b"<!doctype html><title>404</title>"), None);
        assert_eq!(probe(&[]), None);
    }

    #[test]
    fn wav_and_flac_state_their_length() {
        let mut wav = b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x44\xac\0\0\x88\x58\x01\0\x02\0\x10\0data".to_vec();
        wav.extend_from_slice(&88_200u32.to_le_bytes());
        assert_eq!(probe(&wav).map(|p| p.duration), Some(1.0));
        let mut flac = b"fLaC\x80\0\0\x22".to_vec();
        let mut info = [0u8; 34];
        // 44100 Hz, 88200 samples.
        info[10] = (44_100u32 >> 12) as u8;
        info[11] = (44_100u32 >> 4) as u8;
        info[12] = ((44_100u32 & 0x0F) << 4) as u8;
        info[14..18].copy_from_slice(&88_200u32.to_be_bytes());
        flac.extend_from_slice(&info);
        assert_eq!(probe(&flac).map(|p| p.duration), Some(2.0));
    }
}
