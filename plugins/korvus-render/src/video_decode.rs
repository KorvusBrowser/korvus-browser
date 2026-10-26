//! A `<video>`'s decoders (`RenderBackend::video_decoder`): H.264 through OpenH264 (Cisco's decoder, built from the
//! source the `openh264` crate carries), AV1 through `rav1d` (the Rust port of dav1d, Chrome's AV1 decoder, already
//! here for AVIF). VP8 and VP9 have no decoder in this build -- the vendored libvpx is VP8-only and optional
//! (`scripts/vpx-source.sh`) -- so their tracks answer `None` and the element shows its poster.
//!
//! Each frame leaves as 8-bit 4:2:0 YUV with the stream's colour description; the media thread turns the one it
//! shows into RGBA (`YuvFrame::to_rgba`), so the frames a seek decodes and passes over cost no conversion.

use spec::{VideoCodec, VideoDecoder, YuvFrame};

/// The decoder for `codec`, or `None` for a format this build does not decode.
pub fn decoder(codec: &VideoCodec) -> Option<Box<dyn VideoDecoder>> {
    match &codec.fourcc {
        b"avc1" | b"avc3" => H264::new(&codec.config).map(|d| Box::new(d) as Box<dyn VideoDecoder>),
        b"av01" => Av1::new().map(|d| Box::new(d) as Box<dyn VideoDecoder>),
        _ => None,
    }
}

// ---- H.264 ----------------------------------------------------------------------------------------

/// An `avcC` record's parts: the NAL length size, and the parameter sets (SPS and PPS, as NAL units).
struct AvcConfig {
    length_size: usize,
    sps: Vec<Vec<u8>>,
    pps: Vec<Vec<u8>>,
}

fn avc_config(b: &[u8]) -> Option<AvcConfig> {
    if b.len() < 7 || b[0] != 1 {
        return None;
    }
    let length_size = (b[4] & 3) as usize + 1;
    let mut at = 5;
    let sets = |count_mask: u8, at: &mut usize| -> Option<Vec<Vec<u8>>> {
        let n = (*b.get(*at)? & count_mask) as usize;
        *at += 1;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            let len = u16::from_be_bytes([*b.get(*at)?, *b.get(*at + 1)?]) as usize;
            out.push(b.get(*at + 2..*at + 2 + len)?.to_vec());
            *at += 2 + len;
        }
        Some(out)
    };
    let sps = sets(0x1F, &mut at)?;
    let pps = sets(0xFF, &mut at)?;
    Some(AvcConfig { length_size, sps, pps })
}

/// Exp-Golomb reading over an RBSP (emulation prevention bytes removed).
struct Bits<'a> {
    b: &'a [u8],
    at: usize,
}

impl Bits<'_> {
    fn bit(&mut self) -> Option<u32> {
        let byte = *self.b.get(self.at / 8)?;
        let v = (byte >> (7 - self.at % 8)) & 1;
        self.at += 1;
        Some(v as u32)
    }
    fn bits(&mut self, n: u32) -> Option<u32> {
        (0..n).try_fold(0u32, |v, _| Some((v << 1) | self.bit()?))
    }
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        Some((1u32 << zeros) - 1 + self.bits(zeros)?)
    }
    fn se(&mut self) -> Option<i32> {
        let k = self.ue()? as i64;
        Some(if k % 2 == 1 { ((k + 1) / 2) as i32 } else { -(k / 2) as i32 })
    }
}

fn rbsp(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &byte in nal {
        if zeros >= 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        zeros = if byte == 0 { zeros + 1 } else { 0 };
        out.push(byte);
    }
    out
}

/// An SPS's colour description -- the VUI's `matrix_coefficients` and `video_full_range_flag` -- or unspecified,
/// limited range when it carries none.
fn sps_colour(sps: &[u8]) -> (u8, bool) {
    sps_colour_inner(&rbsp(sps)).unwrap_or((2, false))
}

fn sps_colour_inner(b: &[u8]) -> Option<(u8, bool)> {
    let mut r = Bits { b, at: 8 };
    let profile = r.bits(8)?;
    r.bits(16)?; // constraint flags, level
    r.ue()?; // seq_parameter_set_id
    if matches!(profile, 100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135) {
        let chroma = r.ue()?;
        if chroma == 3 {
            r.bit()?;
        }
        r.ue()?;
        r.ue()?;
        r.bit()?;
        if r.bit()? == 1 {
            for i in 0..if chroma == 3 { 12 } else { 8 } {
                if r.bit()? == 1 {
                    let size = if i < 6 { 16 } else { 64 };
                    let (mut last, mut next) = (8i32, 8i32);
                    for _ in 0..size {
                        if next != 0 {
                            next = (last + r.se()? + 256) % 256;
                        }
                        last = if next == 0 { last } else { next };
                    }
                }
            }
        }
    }
    r.ue()?; // log2_max_frame_num_minus4
    match r.ue()? {
        0 => {
            r.ue()?;
        }
        1 => {
            r.bit()?;
            r.se()?;
            r.se()?;
            for _ in 0..r.ue()? {
                r.se()?;
            }
        }
        _ => {}
    }
    r.ue()?; // max_num_ref_frames
    r.bit()?;
    r.ue()?;
    r.ue()?;
    if r.bit()? == 0 {
        r.bit()?; // mb_adaptive_frame_field_flag
    }
    r.bit()?;
    if r.bit()? == 1 {
        r.ue()?;
        r.ue()?;
        r.ue()?;
        r.ue()?;
    }
    if r.bit()? == 0 {
        return None;
    }
    // vui_parameters
    if r.bit()? == 1 && r.bits(8)? == 255 {
        r.bits(32)?;
    }
    if r.bit()? == 1 {
        r.bit()?;
    }
    if r.bit()? == 1 {
        r.bits(3)?;
        let full = r.bit()? == 1;
        if r.bit()? == 1 {
            r.bits(16)?; // colour_primaries, transfer_characteristics
            return Some((r.bits(8)? as u8, full));
        }
        return Some((2, full));
    }
    None
}

struct H264 {
    decoder: openh264::decoder::Decoder,
    config: AvcConfig,
    colour: (u8, bool),
    /// The parameter sets go in before the first sample (and again after a flush).
    primed: bool,
}

impl H264 {
    fn new(config: &[u8]) -> Option<Self> {
        let config = avc_config(config)?;
        let colour = config.sps.first().map(|s| sps_colour(s)).unwrap_or((2, false));
        let api = openh264::OpenH264API::from_source();
        let cfg = openh264::decoder::DecoderConfig::new().flush_after_decode(openh264::decoder::Flush::NoFlush);
        let decoder = openh264::decoder::Decoder::with_api_config(api, cfg).ok()?;
        Some(H264 { decoder, config, colour, primed: false })
    }

    /// A sample's length-prefixed NAL units as an Annex B byte stream, led by the parameter sets when the decoder
    /// has not had them.
    fn annex_b(&mut self, sample: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(sample.len() + 64);
        if !self.primed {
            for set in self.config.sps.iter().chain(&self.config.pps) {
                out.extend_from_slice(&[0, 0, 0, 1]);
                out.extend_from_slice(set);
            }
            self.primed = true;
        }
        let n = self.config.length_size;
        let mut at = 0;
        while at + n <= sample.len() {
            let len = sample[at..at + n].iter().fold(0usize, |v, b| (v << 8) | *b as usize);
            at += n;
            let Some(nal) = sample.get(at..at + len) else { break };
            // an in-band SPS (`avc3`) brings its own colour description
            if nal.first().is_some_and(|h| h & 0x1F == 7) {
                self.colour = sps_colour(nal);
            }
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(nal);
            at += len;
        }
        out
    }
}

impl VideoDecoder for H264 {
    fn decode(&mut self, sample: &[u8]) -> Vec<YuvFrame> {
        let stream = self.annex_b(sample);
        let colour = self.colour;
        match self.decoder.decode(&stream) {
            Ok(Some(yuv)) => vec![H264::frame_with(&yuv, colour)],
            _ => Vec::new(),
        }
    }

    fn flush(&mut self) -> Vec<YuvFrame> {
        let colour = self.colour;
        let frames = match self.decoder.flush_remaining() {
            Ok(frames) => frames.iter().map(|y| H264::frame_with(y, colour)).collect(),
            Err(_) => Vec::new(),
        };
        self.primed = false;
        frames
    }
}

impl H264 {
    /// A decoded picture's planes, copied out of the decoder at their own size.
    fn frame_with(yuv: &openh264::decoder::DecodedYUV, colour: (u8, bool)) -> YuvFrame {
        use openh264::formats::YUVSource;
        let (w, h) = yuv.dimensions();
        let (sy, su, sv) = yuv.strides();
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let plane = |src: &[u8], stride: usize, pw: usize, ph: usize| -> Vec<u8> {
            let mut out = Vec::with_capacity(pw * ph);
            for row in 0..ph {
                let start = row * stride;
                match src.get(start..start + pw) {
                    Some(r) => out.extend_from_slice(r),
                    None => out.resize(out.len() + pw, 0),
                }
            }
            out
        };
        YuvFrame { width: w as u32, height: h as u32, y: plane(yuv.y(), sy, w, h), u: plane(yuv.u(), su, cw, ch), v: plane(yuv.v(), sv, cw, ch), matrix: colour.0, full_range: colour.1 }
    }
}

// ---- AV1 ------------------------------------------------------------------------------------------

struct Av1 {
    ctx: Option<rav1d::include::dav1d::dav1d::Dav1dContext>,
}

// SAFETY: the context is only ever used from the one thread that owns the decoder at a time (the media thread);
// rav1d's context is itself `Send` data behind its handle.
unsafe impl Send for Av1 {}

impl Av1 {
    fn new() -> Option<Self> {
        use rav1d::include::dav1d::dav1d::Dav1dSettings;
        use rav1d::src::lib::{dav1d_default_settings, dav1d_open};
        use std::ptr::NonNull;
        // SAFETY: the dav1d API as rav1d's tests drive it -- settings defaulted before use, one context opened.
        unsafe {
            let mut settings = std::mem::MaybeUninit::<Dav1dSettings>::uninit();
            dav1d_default_settings(NonNull::new(settings.as_mut_ptr())?);
            let mut settings = settings.assume_init();
            // frame threading as well as tile threading (dav1d's default delay): a 60 fps stream at 2K -- a Vimeo
            // background video's top rendition -- decodes at its rate only with frames in flight together
            settings.n_threads = std::thread::available_parallelism().map_or(2, |n| n.get().min(8)) as _;
            settings.max_frame_delay = 0;
            let mut ctx = None;
            if dav1d_open(Some(NonNull::from(&mut ctx)), Some(NonNull::from(&mut settings))).0 != 0 {
                return None;
            }
            Some(Av1 { ctx })
        }
    }

    /// The pictures the decoder has ready.
    fn drain(&mut self, out: &mut Vec<YuvFrame>) {
        use rav1d::include::dav1d::picture::Dav1dPicture;
        use rav1d::src::lib::{dav1d_get_picture, dav1d_picture_unref};
        use std::ptr::NonNull;
        loop {
            let mut pic = Dav1dPicture::default();
            // SAFETY: a live context, a default picture the call fills, released after it is copied.
            let got = unsafe { dav1d_get_picture(self.ctx.clone(), Some(NonNull::from(&mut pic))).0 };
            if got != 0 {
                return;
            }
            if let Some(f) = unsafe { av1_frame(&pic) } {
                out.push(f);
            }
            unsafe { dav1d_picture_unref(Some(NonNull::from(&mut pic))) };
        }
    }
}

impl VideoDecoder for Av1 {
    fn decode(&mut self, sample: &[u8]) -> Vec<YuvFrame> {
        use rav1d::include::dav1d::data::Dav1dData;
        use rav1d::src::lib::{dav1d_data_create, dav1d_data_unref, dav1d_send_data};
        use std::ptr::NonNull;
        let mut out = Vec::new();
        if sample.is_empty() {
            return out;
        }
        // SAFETY: one data buffer created at the sample's size and filled before it is sent; what the decoder did
        // not take (EAGAIN: its picture queue is full) goes in again once the pictures are drained.
        unsafe {
            let mut data = Dav1dData::default();
            let buf = dav1d_data_create(Some(NonNull::from(&mut data)), sample.len());
            if buf.is_null() {
                return out;
            }
            std::ptr::copy_nonoverlapping(sample.as_ptr(), buf, sample.len());
            for _ in 0..64 {
                let sent = dav1d_send_data(self.ctx.clone(), Some(NonNull::from(&mut data))).0;
                self.drain(&mut out);
                if sent != -11 || data.sz == 0 {
                    break;
                }
            }
            if data.sz > 0 {
                dav1d_data_unref(Some(NonNull::from(&mut data)));
            }
        }
        out
    }

    fn flush(&mut self) -> Vec<YuvFrame> {
        let mut out = Vec::new();
        self.drain(&mut out);
        // SAFETY: a live context; flushing drops what it still holds so the next keyframe starts clean.
        if let Some(ctx) = self.ctx.clone() {
            unsafe { rav1d::src::lib::dav1d_flush(ctx) };
        }
        out
    }
}

impl Drop for Av1 {
    fn drop(&mut self) {
        // SAFETY: the context opened in `new`, closed once.
        unsafe { rav1d::src::lib::dav1d_close(Some(std::ptr::NonNull::from(&mut self.ctx))) };
    }
}

/// A rav1d picture as 8-bit 4:2:0: deeper samples shifted down, 4:2:2 and 4:4:4 chroma averaged to 4:2:0, monochrome
/// given neutral chroma.
unsafe fn av1_frame(pic: &rav1d::include::dav1d::picture::Dav1dPicture) -> Option<YuvFrame> {
    use rav1d::include::dav1d::headers::{DAV1D_PIXEL_LAYOUT_I400, DAV1D_PIXEL_LAYOUT_I420, DAV1D_PIXEL_LAYOUT_I422};
    let (w, h) = (pic.p.w as usize, pic.p.h as usize);
    if w == 0 || h == 0 {
        return None;
    }
    let shift = (pic.p.bpc as u32).saturating_sub(8);
    let wide = pic.p.bpc > 8;
    let (sx, sy) = match pic.p.layout {
        DAV1D_PIXEL_LAYOUT_I400 => (9, 9),
        DAV1D_PIXEL_LAYOUT_I420 => (1, 1),
        DAV1D_PIXEL_LAYOUT_I422 => (1, 0),
        _ => (0, 0),
    };
    let sample = |plane: usize, x: usize, y: usize, stride: isize| -> u8 {
        let Some(base) = pic.data[plane] else { return 128 };
        // SAFETY: rav1d's planes are `stride` bytes per row, covering the picture at the plane's subsampling.
        unsafe {
            let row = (base.as_ptr() as *const u8).offset(y as isize * stride);
            if wide { (*(row as *const u16).add(x) >> shift).min(255) as u8 } else { *row.add(x) }
        }
    };
    let mut y_plane = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            y_plane.push(sample(0, x, y, pic.stride[0]));
        }
    }
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let (mut u, mut v) = (Vec::with_capacity(cw * ch), Vec::with_capacity(cw * ch));
    for cy in 0..ch {
        for cx in 0..cw {
            if sx == 9 {
                u.push(128);
                v.push(128);
                continue;
            }
            // the source chroma samples this 4:2:0 sample covers
            let (pw, ph) = ((w + sx) >> sx, (h + sy) >> sy);
            let xs: &[usize] = if sx == 1 { &[cx] } else { &[2 * cx, (2 * cx + 1).min(pw - 1)] };
            let ys: &[usize] = if sy == 1 { &[cy] } else { &[2 * cy, (2 * cy + 1).min(ph - 1)] };
            let (mut su, mut sv, mut n) = (0u32, 0u32, 0u32);
            for &yy in ys {
                for &xx in xs {
                    su += sample(1, xx.min(pw - 1), yy.min(ph - 1), pic.stride[1]) as u32;
                    sv += sample(2, xx.min(pw - 1), yy.min(ph - 1), pic.stride[1]) as u32;
                    n += 1;
                }
            }
            u.push(((su + n / 2) / n) as u8);
            v.push(((sv + n / 2) / n) as u8);
        }
    }
    let seq = pic.seq_hdr.map(|s| unsafe { s.as_ref() });
    let matrix = seq.map(|s| s.mtrx as u8).unwrap_or(2);
    let full_range = seq.is_some_and(|s| s.color_range != 0);
    Some(YuvFrame { width: w as u32, height: h as u32, y: y_plane, u, v, matrix, full_range })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An MP4's first video track: its sample entry four-cc, its configuration record, and its samples in decode
    /// order (a tiny reader for the tests; the engine's demuxer lives with the media element).
    fn mp4_track(b: &[u8]) -> ([u8; 4], Vec<u8>, Vec<Vec<u8>>) {
        fn be32(b: &[u8], at: usize) -> usize {
            u32::from_be_bytes(b[at..at + 4].try_into().unwrap()) as usize
        }
        fn kids(b: &[u8]) -> Vec<([u8; 4], &[u8])> {
            let mut out = Vec::new();
            let mut at = 0;
            while at + 8 <= b.len() {
                let size = be32(b, at).max(8).min(b.len() - at);
                out.push((b[at + 4..at + 8].try_into().unwrap(), &b[at + 8..at + size]));
                at += size;
            }
            out
        }
        fn get<'a>(b: &'a [u8], path: &[&[u8; 4]]) -> &'a [u8] {
            path.iter().fold(b, |b, t| kids(b).into_iter().find(|(k, _)| k == *t).unwrap().1)
        }
        let moov = get(b, &[b"moov"]);
        let trak = kids(moov).into_iter().filter(|(t, _)| t == b"trak").map(|(_, t)| t).find(|t| &get(t, &[b"mdia", b"hdlr"])[8..12] == b"vide").unwrap();
        let stbl = get(trak, &[b"mdia", b"minf", b"stbl"]);
        let stsd = get(stbl, &[b"stsd"]);
        let entry = &stsd[8..];
        let fourcc: [u8; 4] = entry[4..8].try_into().unwrap();
        let config = kids(&entry[8 + 78..be32(entry, 0)]).into_iter().find(|(t, _)| matches!(t, b"avcC" | b"av1C")).unwrap().1.to_vec();
        let stsz = get(stbl, &[b"stsz"]);
        let count = be32(stsz, 8);
        let sizes: Vec<usize> = (0..count).map(|i| if be32(stsz, 4) != 0 { be32(stsz, 4) } else { be32(stsz, 12 + 4 * i) }).collect();
        let stco = get(stbl, &[b"stco"]);
        let chunks: Vec<usize> = (0..be32(stco, 4)).map(|i| be32(stco, 8 + 4 * i)).collect();
        let stsc = get(stbl, &[b"stsc"]);
        let runs: Vec<(usize, usize)> = (0..be32(stsc, 4)).map(|i| (be32(stsc, 8 + 12 * i), be32(stsc, 12 + 12 * i))).collect();
        let mut samples = Vec::new();
        let mut s = 0;
        for (ci, &off) in chunks.iter().enumerate() {
            let per = runs.iter().rev().find(|(first, _)| *first <= ci + 1).unwrap().1;
            let mut at = off;
            for _ in 0..per {
                if s < sizes.len() {
                    samples.push(b[at..at + sizes[s]].to_vec());
                    at += sizes[s];
                    s += 1;
                }
            }
        }
        (fourcc, config, samples)
    }

    fn wpt(path: &str) -> Vec<u8> {
        std::fs::read(format!("{}/../../tools/realweb/seed/media/{path}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    fn decode_all(file: &str) -> Vec<YuvFrame> {
        let (fourcc, config, samples) = mp4_track(&wpt(file));
        let mut d = decoder(&VideoCodec { fourcc, config }).expect("a decoder");
        let mut frames: Vec<YuvFrame> = samples.iter().flat_map(|s| d.decode(s)).collect();
        frames.extend(d.flush());
        frames
    }

    #[test]
    fn an_h264_clip_decodes_to_its_colour() {
        // WPT's 2x2-green.mp4: every frame green
        let frames = decode_all("v.mp4");
        assert!(!frames.is_empty());
        // no colour description in its SPS: the player takes BT.601 for an SD file (`video_pipe.rs`)
        assert_eq!(frames[0].matrix, 2);
        let rgba = spec::YuvFrame { matrix: 6, ..frames[0].clone() }.to_rgba();
        assert_eq!((rgba.width, rgba.height), (2, 2));
        let p = &rgba.data[0..4];
        // CSS `green`, rgb(0, 128, 0)
        assert!(p[0] < 10 && (118..138).contains(&p[1]) && p[2] < 10, "{p:?}");
    }

    #[test]
    fn an_h264_clip_gives_every_frame() {
        let (_, _, samples) = mp4_track(&wpt("vectors/h264.mp4"));
        let frames = decode_all("vectors/h264.mp4");
        assert_eq!(frames.len(), samples.len());
        assert_eq!((frames[0].width, frames[0].height), (320, 240));
    }

    #[test]
    fn an_av1_clip_decodes() {
        let (_, _, samples) = mp4_track(&wpt("vectors/av1.mp4"));
        let frames = decode_all("vectors/av1.mp4");
        assert_eq!(frames.len(), samples.len());
        assert_eq!((frames[0].width, frames[0].height), (320, 240));
    }

    #[test]
    fn four_colours_are_their_colours() {
        // WebCodecs' four-colors.mp4 (AV1), read at its corners as `verifyFourColorsImage` reads it: yellow, red, blue,
        // green
        let frames = decode_all("vectors/four-colors.mp4");
        let rgba = frames[0].to_rgba();
        let (w, h) = (rgba.width as usize, rgba.height as usize);
        let at = |x: usize, y: usize| rgba.data[(y * w + x) * 4..(y * w + x) * 4 + 3].to_vec();
        let near = |p: Vec<u8>, want: [u8; 3]| p.iter().zip(want).all(|(a, b)| (*a as i32 - b as i32).abs() < 40);
        for ((x, y), want) in [((0, 0), [255, 255, 0]), ((w - 1, 0), [255, 0, 0]), ((0, h - 1), [0, 0, 255]), ((w - 1, h - 1), [0, 255, 0])] {
            assert!(near(at(x, y), want), "({x}, {y}): {:?}", at(x, y));
        }
    }

    #[test]
    fn vp9_has_no_decoder_here() {
        assert!(decoder(&VideoCodec { fourcc: *b"vp09", config: Vec::new() }).is_none());
    }
}
