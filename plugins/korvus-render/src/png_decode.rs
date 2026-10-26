//! PNG decoded as Chrome 150 decodes it.
//!
//! Blink's `PngImageDecoder` is Skia's `SkPngRustDecoder`: the `png` crate -- 0.18.1 and `fdeflate` 0.3.7, the
//! versions Chromium 150 vendors, byte for byte the ones built here -- driven as `skia/rust/png/FFI.rs` drives it:
//! default `DecodeOptions` (chunk CRCs checked, the zlib Adler-32 NOT checked, an ancillary chunk with a bad CRC
//! skipped) with text chunks ignored, the header read up to the first `IDAT` before any pixel is. The prebuilt Skia
//! this backend otherwise decodes with carries libpng, which disagrees on exactly the inputs a challenge is built
//! from: an interactive-widget challenge challenge image (`/cdn/challenge-platform/.../ci/...`) whose `IDAT` is a raw deflate stream
//! with no zlib header is `load` and 65x51 in Chrome, with no pixel to draw, where libpng's whole decode failed.
//!
//! What Blink makes of the two halves (`SkiaImageDecoderBase`, `ImageResourceContent`): the SIZE is known once the
//! header parses, so the image fires `load` and reports its natural size; a frame whose pixel data then fails --
//! an error, or input that ends before the last row -- is a FAILED frame, and draws nothing at all.

/// The header's size: what an `<img>` reports once its request completes, `None` for a broken image. Blink's
/// `SkiaImageDecoderBase::SetSize` refuses a side over 1,000,000 (copied from WebKit's PNG decoder).
pub fn header(bytes: &[u8]) -> Option<(u32, u32)> {
    let reader = decoder(bytes).read_info().ok()?;
    let info = reader.info();
    (info.width <= 1_000_000 && info.height <= 1_000_000).then_some((info.width, info.height))
}

/// The first frame as Blink holds it: UNPREMULTIPLIED RGBA8 as decoded so far, and whether the decode
/// completed. Blink zero-fills a frame before decoding into it and draws a frame whose decode failed with what
/// it has -- measured, stock Chrome 150: a PNG cut inside its last chunk draws its decoded rows; one whose pixel
/// data cannot begin to decode draws transparent black -- while `createImageBitmap` takes only a COMPLETE frame.
/// Low bit depths and `tRNS` are expanded and 16-bit samples keep their high byte -- the 8-bit values Skia's
/// swizzler writes for each; no colour transform is applied, as Blink applies none to an image drawn into a canvas
/// without an embedded profile (the backend's other decoder reads pixels the same way). `None` when the header
/// does not parse (a broken image).
pub fn frame(bytes: &[u8]) -> Option<(spec::Rgba8, bool)> {
    let (w, h) = header(bytes)?;
    let wu = w as usize;
    let mut data = vec![0u8; wu * (h as usize) * 4];
    let mut dec = decoder(bytes);
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let Ok(mut reader) = dec.read_info() else {
        return Some((spec::Rgba8 { width: w, height: h, data }, false));
    };
    let (ct, depth) = reader.output_color_type();
    // One decoded row as RGBA8 (EXPAND and STRIP_16 leave only 8-bit grey, grey+alpha, RGB and RGBA).
    let to_rgba = |line: &[u8], out: &mut [u8]| -> bool {
        match (ct, depth) {
            (png::ColorType::Grayscale, png::BitDepth::Eight) => line.iter().take(wu).zip(out.chunks_exact_mut(4)).for_each(|(&g, o)| o.copy_from_slice(&[g, g, g, 255])),
            (png::ColorType::GrayscaleAlpha, png::BitDepth::Eight) => line.chunks_exact(2).take(wu).zip(out.chunks_exact_mut(4)).for_each(|(p, o)| o.copy_from_slice(&[p[0], p[0], p[0], p[1]])),
            (png::ColorType::Rgb, png::BitDepth::Eight) => line.chunks_exact(3).take(wu).zip(out.chunks_exact_mut(4)).for_each(|(p, o)| o.copy_from_slice(&[p[0], p[1], p[2], 255])),
            (png::ColorType::Rgba, png::BitDepth::Eight) => line.chunks_exact(4).take(wu).zip(out.chunks_exact_mut(4)).for_each(|(p, o)| o.copy_from_slice(p)),
            _ => return false,
        }
        true
    };
    let Some(line) = reader.output_line_size(w) else {
        return Some((spec::Rgba8 { width: w, height: h, data }, false));
    };
    let mut row = vec![0u8; line];
    let mut rgba = vec![0u8; wu * 4];
    let mut y = 0usize;
    // `read_row` until it says the frame is over, as `SkPngRustCodec::incrementalDecode` does: the frame is
    // COMPLETE only then -- the last row is not enough (the call after it reads the rest of the frame's data, and
    // a stream cut after its last row fails there). An Adam7 pass row lands where its pixels go
    // (`png::expand_interlaced_row`, what Skia's `expandDecodedInterlacedRow` calls).
    let complete = loop {
        match reader.read_row(&mut row) {
            Ok(Some(png::InterlaceInfo::Null(_))) => {
                if y >= h as usize || !to_rgba(&row, &mut rgba) {
                    break false;
                }
                data[y * wu * 4..(y + 1) * wu * 4].copy_from_slice(&rgba);
                y += 1;
            }
            Ok(Some(png::InterlaceInfo::Adam7(info))) => {
                if !to_rgba(&row, &mut rgba) {
                    break false;
                }
                png::expand_interlaced_row(&mut data, wu * 4, &rgba, &info, 32);
            }
            Ok(None) => break true,
            Err(_) => break false,
        }
    };
    Some((spec::Rgba8 { width: w, height: h, data }, complete))
}

/// The whole first frame, or `None` when Chrome's decode of it does not complete -- what `createImageBitmap`
/// needs.
pub fn decode(bytes: &[u8]) -> Option<spec::Rgba8> {
    frame(bytes).and_then(|(img, complete)| complete.then_some(img))
}

/// Whether `bytes` start with the PNG signature -- how Blink's `ImageDecoder` picks the PNG decoder (by sniffing,
/// whatever the response's type says).
pub fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
}

fn decoder(bytes: &[u8]) -> png::Decoder<std::io::Cursor<&[u8]>> {
    let mut options = png::DecodeOptions::default();
    options.set_ignore_text_chunk(true);
    png::Decoder::new_with_options(std::io::Cursor::new(bytes), options)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PNG with one `IDAT` holding `raw` (filtered scanlines) as `zlib` says: a stored deflate block, with or
    /// without the zlib header and Adler-32.
    fn png(w: u32, h: u32, color: u8, raw: &[u8], zlib_wrapper: bool, adler: Option<u32>) -> Vec<u8> {
        fn chunk(out: &mut Vec<u8>, ty: &[u8], data: &[u8]) {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let mut c = ty.to_vec();
            c.extend_from_slice(data);
            out.extend_from_slice(&c);
            out.extend_from_slice(&crc32(&c).to_be_bytes());
        }
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[8, color, 0, 0, 0]);
        chunk(&mut out, b"IHDR", &ihdr);
        let mut idat = Vec::new();
        if zlib_wrapper {
            idat.extend_from_slice(&[0x78, 0x01]);
        }
        let n = raw.len() as u16;
        idat.push(1);
        idat.extend_from_slice(&n.to_le_bytes());
        idat.extend_from_slice(&(!n).to_le_bytes());
        idat.extend_from_slice(raw);
        if zlib_wrapper {
            idat.extend_from_slice(&adler.unwrap_or_else(|| adler32(raw)).to_be_bytes());
        }
        chunk(&mut out, b"IDAT", &idat);
        chunk(&mut out, b"IEND", &[]);
        out
    }

    fn crc32(b: &[u8]) -> u32 {
        let mut c = 0xffff_ffffu32;
        for &x in b {
            c ^= x as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }

    fn adler32(b: &[u8]) -> u32 {
        let (mut a, mut s) = (1u32, 0u32);
        for &x in b {
            a = (a + x as u32) % 65521;
            s = (s + a) % 65521;
        }
        (s << 16) | a
    }

    /// Two grey rows, filter type 0.
    const GREY_2X2: [u8; 6] = [0, 10, 20, 0, 30, 40];

    #[test]
    fn a_valid_png_decodes_to_its_pixels() {
        let p = png(2, 2, 0, &GREY_2X2, true, None);
        assert_eq!(header(&p), Some((2, 2)));
        let img = decode(&p).expect("decodes");
        assert_eq!(img.data, vec![10, 10, 10, 255, 20, 20, 20, 255, 30, 30, 30, 255, 40, 40, 40, 255]);
    }

    /// Chrome's decoder does not check the zlib Adler-32 (`DecodeOptions::default`), so a wrong one decodes --
    /// libpng's decode of it fails.
    #[test]
    fn a_wrong_adler32_still_decodes() {
        let p = png(2, 2, 0, &GREY_2X2, true, Some(0xdead_beef));
        assert!(decode(&p).is_some());
    }

    /// a bot challenge image's shape: an `IDAT` that is raw deflate with no zlib header. The header
    /// parses -- `load`, the size -- and the pixel data does not: nothing to draw (stock Chrome 150, measured).
    #[test]
    fn an_idat_without_its_zlib_header_has_a_size_and_no_pixels() {
        let p = png(2, 2, 0, &GREY_2X2, false, None);
        assert_eq!(header(&p), Some((2, 2)));
        assert!(decode(&p).is_none());
    }

    /// Input that ends before the frame does is an INCOMPLETE frame: its decoded rows are drawn, nothing else is
    /// taken. The bytes are the 2x2 PNG measured on stock Chrome 150 (zlib-compressed, cut 20 bytes short: its
    /// Adler-32, its IDAT CRC and its IEND gone): `load`, 2x2, drawImage shows the first row, createImageBitmap
    /// of it is "The ImageBitmap could not be allocated." and of its Blob "could not be decoded".
    #[test]
    fn a_truncated_png_keeps_the_rows_it_decoded() {
        const GOOD: &str = "89504e470d0a1a0a0000000d494844520000000200000002080000000057dd52f80000000e49444154789c63e0126190d3000000ec0065e0f85cd30000000049454e44ae426082";
        let good: Vec<u8> = (0..GOOD.len()).step_by(2).map(|i| u8::from_str_radix(&GOOD[i..i + 2], 16).unwrap()).collect();
        assert_eq!(decode(&good).unwrap().data, vec![10, 10, 10, 255, 20, 20, 20, 255, 30, 30, 30, 255, 40, 40, 40, 255]);
        let cut = &good[..good.len() - 20];
        assert_eq!(header(cut), Some((2, 2)));
        assert!(decode(cut).is_none());
        let (img, complete) = frame(cut).unwrap();
        assert!(!complete);
        assert_eq!(&img.data[..8], &[10, 10, 10, 255, 20, 20, 20, 255]);
    }

    /// A frame whose pixel data cannot begin to decode is all transparent black.
    #[test]
    fn an_undecodable_frame_is_transparent() {
        let p = png(2, 2, 0, &GREY_2X2, false, None);
        let (img, complete) = frame(&p).unwrap();
        assert!(!complete);
        assert!(img.data.iter().all(|&b| b == 0));
    }

    #[test]
    fn a_bad_ihdr_crc_is_a_broken_image() {
        let mut p = png(2, 2, 0, &GREY_2X2, true, None);
        p[29] ^= 1; // a byte of the IHDR CRC
        assert_eq!(header(&p), None);
    }
}
