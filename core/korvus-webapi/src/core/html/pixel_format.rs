//! `core/html/pixel_format.rs` -- `ImageDataSettings` as Blink reads it, and an `ImageData`'s pixel formats.
//!
//! Chrome 150 (`image_data_settings.idl`, `image_data.cc`) gives an `ImageData` one of three pixel formats --
//! `rgba-unorm8` (a `Uint8ClampedArray`), `rgba-float16` (a `Float16Array`), `rgba-float32` (a `Float32Array`) --
//! and every `ImageDataSettings` reader (`new ImageData`, `getImageData`, `createImageData`) converts the
//! dictionary in two phases: the BINDING's enum check while the arguments are converted (`colorSpace`, then
//! `pixelFormat`), and later, inside the operation, the colour spaces the IDL lists but a runtime flag keeps off
//! (`rec2020`, `rec2100-*`, the linear ones), each with its own sentence. The engine read no `pixelFormat` at all:
//! a bot challenge reads a canvas back as `rgba-float16` and hashes the bytes, which were `unorm8`'s.

use deno_core::v8;

use crate::binding::protos::{get_prop, throw_type_error};

/// An `ImageData`'s pixel format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PixelFormat {
    Unorm8,
    Float16,
    Float32,
}

impl PixelFormat {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            PixelFormat::Unorm8 => "rgba-unorm8",
            PixelFormat::Float16 => "rgba-float16",
            PixelFormat::Float32 => "rgba-float32",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "rgba-unorm8" => Some(PixelFormat::Unorm8),
            "rgba-float16" => Some(PixelFormat::Float16),
            "rgba-float32" => Some(PixelFormat::Float32),
            _ => None,
        }
    }

    /// The typed array a data array of this format is (`Float16Array must use rgba-float16 pixelFormat.`).
    pub(crate) fn array_name(self) -> &'static str {
        match self {
            PixelFormat::Unorm8 => "Uint8ClampedArray",
            PixelFormat::Float16 => "Float16Array",
            PixelFormat::Float32 => "Float32Array",
        }
    }

    /// The format a data array's own kind stands for -- `None` for any array an `ImageData` cannot hold.
    pub(crate) fn of_array(v: v8::Local<v8::Value>) -> Option<Self> {
        if v.is_uint8_clamped_array() {
            Some(PixelFormat::Unorm8)
        } else if v.is_float16_array() {
            Some(PixelFormat::Float16)
        } else if v.is_float32_array() {
            Some(PixelFormat::Float32)
        } else {
            None
        }
    }
}

/// `PredefinedColorSpace`'s IDL values: the binding accepts these and refuses anything else.
pub(crate) const IDL_COLOR_SPACES: [&str; 8] =
    ["srgb", "rec2020", "display-p3", "rec2100-hlg", "rec2100-pq", "srgb-linear", "display-p3-linear", "rec2100-linear"];

/// The colour spaces Chrome 150 has ENABLED (the rest are behind CanvasHDR / ColorSpacePredefinedLinearSpaces).
pub(crate) fn color_space_enabled(cs: &str) -> bool {
    cs == "srgb" || cs == "display-p3"
}

/// An `ImageDataSettings` dictionary's two members, read as the binding reads them -- `colorSpace` then
/// `pixelFormat`, each enum-checked -- `None` once a TypeError is thrown (`prefix` is the operation's "Failed to
/// ... : "). A member that is absent is `None` in the pair; `pixelFormat`'s IDL default is the caller's.
pub(crate) fn read_settings(
    scope: &mut v8::PinScope,
    settings: Option<v8::Local<v8::Object>>,
    prefix: &str,
) -> Option<(Option<String>, Option<PixelFormat>)> {
    let Some(s) = settings else { return Some((None, None)) };
    let mut out = (None, None);
    for member in ["colorSpace", "pixelFormat"] {
        let v = get_prop(scope, s, member)?;
        if v.is_undefined() {
            continue;
        }
        let text = v.to_string(scope)?.to_rust_string_lossy(scope);
        let (ty, ok) = if member == "colorSpace" {
            ("PredefinedColorSpace", IDL_COLOR_SPACES.contains(&text.as_str()))
        } else {
            ("ImageDataPixelFormat", PixelFormat::parse(&text).is_some())
        };
        if !ok {
            throw_type_error(
                scope,
                &format!("{prefix}Failed to read the '{member}' property from 'ImageDataSettings': The provided value '{text}' is not a valid enum value of type {ty}."),
            );
            return None;
        }
        if member == "colorSpace" {
            out.0 = Some(text);
        } else {
            out.1 = PixelFormat::parse(&text);
        }
    }
    Some(out)
}

/// The runtime check of a colour space the binding let through: a disabled one throws Blink's
/// `ValidateAndConvertColorSpace` sentence. `false` once thrown.
pub(crate) fn check_color_space_enabled(scope: &mut v8::PinScope, cs: &str, prefix: &str) -> bool {
    if color_space_enabled(cs) {
        return true;
    }
    throw_type_error(scope, &format!("{prefix}The provided value '{cs}' is not a valid enum value of the type PredefinedColorSpace."));
    false
}

/// `f32` -> IEEE half, rounded to nearest even (what a `Float16Array` stores).
pub(crate) fn f32_to_f16(v: f32) -> u16 {
    let bits = v.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xff) as i32;
    let man = bits & 0x7f_ffff;
    if exp == 0xff {
        return sign | 0x7c00 | if man != 0 { 0x200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let m = man | 0x80_0000;
        let shift = (14 - e) as u32;
        let half = 1u32 << (shift - 1);
        let rest = m & ((1u32 << shift) - 1);
        let mut h = (m >> shift) as u16;
        if rest > half || (rest == half && h & 1 == 1) {
            h += 1;
        }
        return sign | h;
    }
    let mut h = ((e as u32) << 10 | (man >> 13)) as u16;
    let rest = man & 0x1fff;
    if rest > 0x1000 || (rest == 0x1000 && h & 1 == 1) {
        h += 1;
    }
    sign | h
}

/// IEEE half -> `f32`.
pub(crate) fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h & 0x8000) as u32) << 16;
    let exp = ((h >> 10) & 0x1f) as u32;
    let man = (h & 0x3ff) as u32;
    let bits = match exp {
        0 if man == 0 => sign,
        0 => {
            let mut e = 127 - 15 + 1;
            let mut m = man;
            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }
            sign | (e << 23) | ((m & 0x3ff) << 13)
        }
        0x1f => sign | 0x7f80_0000 | (man << 13),
        _ => sign | ((exp + 127 - 15) << 23) | (man << 13),
    };
    f32::from_bits(bits)
}

/// A canvas's UNPREMULTIPLIED RGBA8 read back as Chrome reads a float `ImageData` from its premultiplied
/// backing: alpha `A/255`; a colour channel `p/A`, where `p` is the premultiplied byte the backing holds (recovered
/// exactly from the unpremultiplied one, `round(C*A/255)`, since the unpremultiply rounded to nearest) -- not
/// `C/255`: measured, a pixel read back as `[199, 99, 50, 77]` in unorm8 is `[0.7793, 0.3896, 0.1948, 0.3020]` in
/// float16 (60/77, 30/77, 15/77, 77/255).
pub(crate) fn rgba8_to_float(data: &[u8]) -> Vec<f32> {
    let mut out = Vec::with_capacity(data.len());
    for px in data.chunks_exact(4) {
        let a = px[3] as u32;
        for &c in &px[..3] {
            out.push(match a {
                255 => c as f32 / 255.0,
                0 => 0.0,
                _ => ((c as u32 * a + 127) / 255) as f32 / a as f32,
            });
        }
        out.push(a as f32 / 255.0);
    }
    out
}

/// Float channels -> RGBA8, as `putImageData` stores a float `ImageData`: `round(v * 255)` clamped (Chrome 150:
/// 0.002 -> 1, 0.25 -> 64, 1.5 -> 255, -0.5 -> 0).
pub(crate) fn float_to_rgba8(vals: &[f32]) -> Vec<u8> {
    vals.iter().map(|&v| if v.is_nan() { 0 } else { (v * 255.0).round().clamp(0.0, 255.0) as u8 }).collect()
}

/// A fresh typed array of `fmt` over `values` (one per channel); for `Unorm8`, `bytes` are taken as they are.
pub(crate) fn new_data_array<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    fmt: PixelFormat,
    bytes: Option<&[u8]>,
    len: usize,
) -> Option<v8::Local<'s, v8::Value>> {
    match fmt {
        PixelFormat::Unorm8 => {
            let mut b = vec![0u8; len];
            if let Some(src) = bytes {
                let n = src.len().min(len);
                b[..n].copy_from_slice(&src[..n]);
            }
            let store = v8::ArrayBuffer::new_backing_store_from_vec(b).make_shared();
            let ab = v8::ArrayBuffer::with_backing_store(scope, &store);
            v8::Uint8ClampedArray::new(scope, ab, 0, len).map(|a| a.into())
        }
        PixelFormat::Float16 => {
            let floats = bytes.map(rgba8_to_float).unwrap_or_default();
            let mut b = vec![0u8; len * 2];
            for (i, f) in floats.iter().take(len).enumerate() {
                b[i * 2..i * 2 + 2].copy_from_slice(&f32_to_f16(*f).to_le_bytes());
            }
            let store = v8::ArrayBuffer::new_backing_store_from_vec(b).make_shared();
            let ab = v8::ArrayBuffer::with_backing_store(scope, &store);
            // SAFETY: an upcast -- a Float16Array IS a Value (rusty_v8 has no `From` for this one typed array,
            // and `cast_unchecked` is the handle transmute every `From` is).
            v8::Float16Array::new(scope, ab, 0, len).map(|a| unsafe { v8::Local::<v8::Value>::cast_unchecked(a) })
        }
        PixelFormat::Float32 => {
            let floats = bytes.map(rgba8_to_float).unwrap_or_default();
            let mut b = vec![0u8; len * 4];
            for (i, f) in floats.iter().take(len).enumerate() {
                b[i * 4..i * 4 + 4].copy_from_slice(&f.to_le_bytes());
            }
            let store = v8::ArrayBuffer::new_backing_store_from_vec(b).make_shared();
            let ab = v8::ArrayBuffer::with_backing_store(scope, &store);
            v8::Float32Array::new(scope, ab, 0, len).map(|a| a.into())
        }
    }
}

/// A fresh typed array of `fmt` over `raw`, which is already in its layout -- bytes, or little-endian halves or
/// floats, as a render backend's `read_2d` hands them back -- `len` channels long.
pub(crate) fn new_raw_array<'s>(scope: &mut v8::PinScope<'s, '_>, fmt: PixelFormat, mut raw: Vec<u8>, len: usize) -> Option<v8::Local<'s, v8::Value>> {
    let width = match fmt {
        PixelFormat::Unorm8 => 1,
        PixelFormat::Float16 => 2,
        PixelFormat::Float32 => 4,
    };
    raw.resize(len * width, 0);
    let store = v8::ArrayBuffer::new_backing_store_from_vec(raw).make_shared();
    let ab = v8::ArrayBuffer::with_backing_store(scope, &store);
    match fmt {
        PixelFormat::Unorm8 => v8::Uint8ClampedArray::new(scope, ab, 0, len).map(|a| a.into()),
        // SAFETY: an upcast, as in `new_data_array`.
        PixelFormat::Float16 => v8::Float16Array::new(scope, ab, 0, len).map(|a| unsafe { v8::Local::<v8::Value>::cast_unchecked(a) }),
        PixelFormat::Float32 => v8::Float32Array::new(scope, ab, 0, len).map(|a| a.into()),
    }
}

/// The RGBA8 bytes of an `ImageData`'s `data` array of any format (`need` channels), or `None` when it holds fewer.
pub(crate) fn data_array_rgba8(v: v8::Local<v8::Value>, need: usize) -> Option<Vec<u8>> {
    let view = v8::Local::<v8::ArrayBufferView>::try_from(v).ok()?;
    let mut raw = vec![0u8; view.byte_length()];
    view.copy_contents(&mut raw);
    match PixelFormat::of_array(v)? {
        PixelFormat::Unorm8 => {
            (raw.len() >= need).then(|| {
                raw.truncate(need);
                raw
            })
        }
        PixelFormat::Float16 => {
            if raw.len() < need * 2 {
                return None;
            }
            let vals: Vec<f32> = raw.chunks_exact(2).take(need).map(|b| f16_to_f32(u16::from_le_bytes([b[0], b[1]]))).collect();
            Some(float_to_rgba8(&vals))
        }
        PixelFormat::Float32 => {
            if raw.len() < need * 4 {
                return None;
            }
            let vals: Vec<f32> = raw.chunks_exact(4).take(need).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
            Some(float_to_rgba8(&vals))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The half values stock Chrome 150 reads back for these bytes (getImageData, `rgba-float16`).
    #[test]
    fn bytes_read_back_as_chromes_halves() {
        let want = [(0u8, 0.0f32), (1, 0.0039215087890625), (2, 0.007843017578125), (63, 0.2470703125), (64, 0.2509765625),
                    (127, 0.498046875), (128, 0.501953125), (129, 0.505859375), (191, 0.7490234375), (254, 0.99609375), (255, 1.0)];
        for (b, v) in want {
            let f = rgba8_to_float(&[b, b, b, 255]);
            assert_eq!(f16_to_f32(f32_to_f16(f[0])), v, "byte {b}");
        }
    }

    /// The premultiplied backing, unpremultiplied in float (stock Chrome 150, measured).
    #[test]
    fn a_translucent_pixel_reads_back_from_its_premultiplied_bytes() {
        let f: Vec<f32> = rgba8_to_float(&[199, 99, 50, 77]).iter().map(|v| f16_to_f32(f32_to_f16(*v))).collect();
        assert_eq!(f, vec![0.779296875, 0.3896484375, 0.19482421875, 0.302001953125]);
    }

    /// putImageData of float values, as stock Chrome 150 stores them.
    #[test]
    fn floats_store_as_chromes_bytes() {
        let vals = [0.0f32, 0.001, 0.002, 0.25, 0.4999, 0.5, 0.5001, 0.75, 0.999, 1.0, 1.5, -0.5];
        let halves: Vec<f32> = vals.iter().map(|v| f16_to_f32(f32_to_f16(*v))).collect();
        assert_eq!(float_to_rgba8(&halves), vec![0, 0, 1, 64, 128, 128, 128, 191, 255, 255, 255, 0]);
    }

    #[test]
    fn half_round_trips_and_rounds_to_even() {
        for h in [0u16, 1, 0x3c00, 0x3800, 0x7bff, 0x0400, 0x8001] {
            assert_eq!(f32_to_f16(f16_to_f32(h)), h);
        }
        // halfway between 1.0 and the next half (1 + 2^-10): to even (1.0)
        assert_eq!(f32_to_f16(1.0 + 2f32.powi(-11)), 0x3c00);
        assert_eq!(f32_to_f16(1.0 + 3.0 * 2f32.powi(-11)), 0x3c02);
    }
}
