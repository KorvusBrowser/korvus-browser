//! WebP and AVIF, which Chrome decodes and Skia's build here does not (its WebP decoder is off; AVIF is not a Skia
//! codec): every `<img>`, CSS background and canvas image a site serves in either -- a site picks them because the
//! `Accept` header korvus sends, as Chrome's does, says they are welcome -- drew nothing. minea.com serves 22 of its 25
//! images as AVIF.
//!
//! WebP through `image-webp` (image-rs's pure-Rust decoder); AVIF through `avif-parse` (the container: the colour item,
//! its alpha item) and `rav1d` (the Rust port of dav1d, the AV1 decoder Chrome's libavif uses), without its assembly so
//! no build needs an assembler. Decoded to unpremultiplied RGBA8, as `decode_image` hands every image over.

use spec::Rgba8;

/// Is this a WebP or AVIF body?
pub fn is_modern(bytes: &[u8]) -> bool {
    is_webp(bytes) || is_avif(bytes)
}

fn is_webp(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP"
}

fn is_avif(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[4..8] == b"ftyp" && (&bytes[8..12] == b"avif" || &bytes[8..12] == b"avis")
}

/// A WebP or AVIF image's first frame, or `None` (another format, or one that does not decode).
pub fn decode(bytes: &[u8]) -> Option<Rgba8> {
    if is_webp(bytes) {
        webp(bytes)
    } else if is_avif(bytes) {
        avif(bytes)
    } else {
        None
    }
}

fn webp(bytes: &[u8]) -> Option<Rgba8> {
    let mut d = image_webp::WebPDecoder::new(std::io::Cursor::new(bytes)).ok()?;
    let (w, h) = d.dimensions();
    let alpha = d.has_alpha();
    let mut buf = vec![0u8; d.output_buffer_size()?];
    d.read_image(&mut buf).ok()?;
    let data = if alpha {
        buf
    } else {
        buf.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect()
    };
    Some(Rgba8 { width: w, height: h, data })
}

fn avif(bytes: &[u8]) -> Option<Rgba8> {
    let parsed = avif_parse::read_avif(&mut std::io::Cursor::new(bytes)).ok()?;
    let color = av1::decode(&parsed.primary_item)?;
    let alpha = parsed.alpha_item.as_ref().and_then(|a| av1::decode(a));
    let (w, h) = (color.width, color.height);
    let mut data = vec![0u8; w * h * 4];
    color.write_rgb(&mut data);
    match alpha.filter(|a| a.width == w && a.height == h) {
        Some(a) => {
            for y in 0..h {
                for x in 0..w {
                    let v = a.luma8(x, y, true);
                    let i = (y * w + x) * 4;
                    data[i + 3] = v;
                    if parsed.premultiplied_alpha && v > 0 && v < 255 {
                        for c in 0..3 {
                            data[i + c] = ((data[i + c] as u32 * 255 + v as u32 / 2) / v as u32).min(255) as u8;
                        }
                    }
                }
            }
        }
        None => {
            for px in data.chunks_exact_mut(4) {
                px[3] = 255;
            }
        }
    }
    Some(Rgba8 { width: w as u32, height: h as u32, data })
}

/// One AV1 frame through rav1d's dav1d API.
mod av1 {
    use rav1d::include::dav1d::data::Dav1dData;
    use rav1d::include::dav1d::dav1d::{Dav1dContext, Dav1dSettings};
    use rav1d::include::dav1d::headers::{DAV1D_PIXEL_LAYOUT_I400, DAV1D_PIXEL_LAYOUT_I420, DAV1D_PIXEL_LAYOUT_I422};
    use rav1d::include::dav1d::picture::Dav1dPicture;
    use rav1d::src::lib::{dav1d_close, dav1d_data_create, dav1d_default_settings, dav1d_get_picture, dav1d_open, dav1d_picture_unref, dav1d_send_data};
    use std::ptr::NonNull;

    /// A decoded frame's planes, copied out of the decoder.
    pub struct Frame {
        pub width: usize,
        pub height: usize,
        /// Bits per sample: 8, 10 or 12.
        bpc: u32,
        /// Chroma subsampling: (1, 1) for 4:2:0, (1, 0) for 4:2:2, (0, 0) for 4:4:4; `None` for monochrome.
        sub: Option<(u32, u32)>,
        planes: [Vec<u16>; 3],
        chroma_w: usize,
        /// The matrix coefficients (AV1 `matrix_coefficients`) and whether the samples use the full range.
        matrix: u32,
        full: bool,
    }

    pub fn decode(obus: &[u8]) -> Option<Frame> {
        // SAFETY: the dav1d API as rav1d's tests drive it -- settings defaulted before use, one context opened and
        // closed, one data buffer created at the OBUs' size and filled before it is sent, one picture taken and
        // released; every pointer handed over points at a live local.
        unsafe {
            let mut settings = std::mem::MaybeUninit::<Dav1dSettings>::uninit();
            dav1d_default_settings(NonNull::new(settings.as_mut_ptr())?);
            let mut settings = settings.assume_init();
            settings.n_threads = 1;
            settings.max_frame_delay = 1;
            let mut ctx: Option<Dav1dContext> = None;
            if dav1d_open(Some(NonNull::from(&mut ctx)), Some(NonNull::from(&mut settings))).0 != 0 {
                return None;
            }
            let frame = (|| {
                let mut data = Dav1dData::default();
                let buf = dav1d_data_create(Some(NonNull::from(&mut data)), obus.len());
                if buf.is_null() {
                    return None;
                }
                std::ptr::copy_nonoverlapping(obus.as_ptr(), buf, obus.len());
                let sent = dav1d_send_data(ctx.clone(), Some(NonNull::from(&mut data))).0;
                if sent != 0 && sent != -11 {
                    return None;
                }
                let mut pic = Dav1dPicture::default();
                let mut got = dav1d_get_picture(ctx.clone(), Some(NonNull::from(&mut pic))).0;
                if got == -11 {
                    // the decoder wants the stream drained: ask again once the data is in
                    got = dav1d_get_picture(ctx.clone(), Some(NonNull::from(&mut pic))).0;
                }
                if got != 0 {
                    return None;
                }
                let frame = copy_out(&pic);
                dav1d_picture_unref(Some(NonNull::from(&mut pic)));
                frame
            })();
            dav1d_close(Some(NonNull::from(&mut ctx)));
            frame
        }
    }

    /// The picture's planes as `u16` samples.
    unsafe fn copy_out(pic: &Dav1dPicture) -> Option<Frame> {
        let (w, h) = (pic.p.w as usize, pic.p.h as usize);
        let bpc = pic.p.bpc as u32;
        if w == 0 || h == 0 {
            return None;
        }
        let sub = match pic.p.layout {
            DAV1D_PIXEL_LAYOUT_I400 => None,
            DAV1D_PIXEL_LAYOUT_I420 => Some((1, 1)),
            DAV1D_PIXEL_LAYOUT_I422 => Some((1, 0)),
            _ => Some((0, 0)),
        };
        let (cw, ch) = match sub {
            Some((sx, sy)) => ((w + sx as usize) >> sx, (h + sy as usize) >> sy),
            None => (0, 0),
        };
        let wide = bpc > 8;
        let read = |plane: usize, pw: usize, ph: usize, stride: isize| -> Option<Vec<u16>> {
            let base = pic.data[plane]?.as_ptr() as *const u8;
            let mut out = Vec::with_capacity(pw * ph);
            for y in 0..ph {
                // SAFETY: rav1d's planes are `stride` bytes per row and at least `ph` rows of `pw` samples.
                let row = unsafe { base.offset(y as isize * stride) };
                for x in 0..pw {
                    out.push(if wide { unsafe { *(row as *const u16).add(x) } } else { (unsafe { *row.add(x) }) as u16 });
                }
            }
            Some(out)
        };
        let y = read(0, w, h, pic.stride[0])?;
        let (u, v) = if sub.is_some() { (read(1, cw, ch, pic.stride[1])?, read(2, cw, ch, pic.stride[1])?) } else { (Vec::new(), Vec::new()) };
        let seq = pic.seq_hdr.map(|s| unsafe { s.as_ref() });
        let matrix = seq.map(|s| s.mtrx as u32).unwrap_or(1);
        let full = seq.map(|s| s.color_range != 0).unwrap_or(false);
        Some(Frame { width: w, height: h, bpc, sub, planes: [y, u, v], chroma_w: cw, matrix, full })
    }

    impl Frame {
        fn to8(&self, v: u16) -> f32 {
            v as f32 / ((1u32 << self.bpc) - 1) as f32 * 255.0
        }

        /// The luma sample at (x, y) as 0..=255 -- an alpha plane's value (`full`: taken as full range).
        pub fn luma8(&self, x: usize, y: usize, full: bool) -> u8 {
            let v = self.to8(self.planes[0][y * self.width + x]);
            let v = if full || self.full { v } else { (v - 16.0) * 255.0 / 219.0 };
            v.round().clamp(0.0, 255.0) as u8
        }

        /// RGB into `out` (RGBA8, alpha left alone): the frame's matrix and range, chroma taken from the sample
        /// covering each pixel.
        pub fn write_rgb(&self, out: &mut [u8]) {
            // Kr, Kb of the matrix (BT.709 by default; 5, 6: BT.601; 9: BT.2020)
            let (kr, kb) = match self.matrix {
                5 | 6 => (0.299, 0.114),
                9 | 10 => (0.2627, 0.0593),
                _ => (0.2126, 0.0722),
            };
            let kg = 1.0 - kr - kb;
            for y in 0..self.height {
                for x in 0..self.width {
                    let mut l = self.to8(self.planes[0][y * self.width + x]);
                    let i = (y * self.width + x) * 4;
                    let Some((sx, sy)) = self.sub else {
                        let v = if self.full { l } else { (l - 16.0) * 255.0 / 219.0 };
                        let v = v.round().clamp(0.0, 255.0) as u8;
                        out[i..i + 3].copy_from_slice(&[v, v, v]);
                        continue;
                    };
                    let c = (y >> sy) * self.chroma_w + (x >> sx);
                    let mut cb = self.to8(self.planes[1][c]) - 128.0;
                    let mut cr = self.to8(self.planes[2][c]) - 128.0;
                    if self.matrix == 0 {
                        // identity (GBR): Y is G, U is B, V is R
                        let (g, b, r) = (l, cb + 128.0, cr + 128.0);
                        out[i..i + 3].copy_from_slice(&[r.round().clamp(0.0, 255.0) as u8, g.round().clamp(0.0, 255.0) as u8, b.round().clamp(0.0, 255.0) as u8]);
                        continue;
                    }
                    if !self.full {
                        l = (l - 16.0) * 255.0 / 219.0;
                        cb *= 255.0 / 224.0;
                        cr *= 255.0 / 224.0;
                    }
                    let r = l + 2.0 * (1.0 - kr) * cr;
                    let b = l + 2.0 * (1.0 - kb) * cb;
                    let g = (l - kr * r - kb * b) / kg;
                    out[i] = r.round().clamp(0.0, 255.0) as u8;
                    out[i + 1] = g.round().clamp(0.0, 255.0) as u8;
                    out[i + 2] = b.round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn other_formats_are_not_taken() {
        assert!(!is_modern(b"\x89PNG\r\n\x1a\n0000"));
        assert!(decode(b"GIF89a").is_none());
        assert!(is_modern(b"RIFF\0\0\0\0WEBPVP8 "));
        assert!(is_modern(b"\0\0\0\x1cftypavif"));
    }
}
