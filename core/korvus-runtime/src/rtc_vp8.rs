//! `rtc_vp8.rs` -- VP8 encoding for the WebRTC send path (goal A, Slice 1c): a fed RGBA frame to a VP8
//! bitstream, through the VENDORED libvpx (the encoder Chrome/libwebrtc uses, at the revision Chrome
//! pins), so the bytes a peer decodes match what Chrome would produce. Two steps: RGBA -> I420 (BT.601
//! studio range, the libyuv fixed-point libwebrtc capture uses) and libvpx VP8 encode with libwebrtc's
//! realtime configuration.
//!
//! Compiled only under `webrtc-media` (which links the vendored libvpx); the default build has none of it,
//! and at runtime the encoder is constructed only by the sender, which runs only for a connection with a
//! fed sending track whose handshake completed (I9). All libvpx calls are unsafe FFI, wrapped here.
#![allow(dead_code)] // the sender that drives the encoder lands in the final 1c commit; the test exercises it now.

use std::os::raw::{c_int, c_uint, c_ulong};

/// One encoded VP8 frame: the compressed bytes and whether it is a key (intra) frame.
pub(crate) struct EncodedFrame {
    pub data: Vec<u8>,
    pub is_keyframe: bool,
}

// ── RGBA -> I420, BT.601 LIMITED (studio) range, libyuv fixed-point (what libwebrtc capture uses) ──
#[inline]
fn clamp8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}
#[inline]
fn rgb_to_y(r: i32, g: i32, b: i32) -> u8 {
    clamp8((66 * r + 129 * g + 25 * b + 0x1080) >> 8) // +((16<<8)+128)
}
#[inline]
fn rgb_to_u(r: i32, g: i32, b: i32) -> u8 {
    clamp8((-38 * r - 74 * g + 112 * b + 0x8080) >> 8) // +((128<<8)+128)
}
#[inline]
fn rgb_to_v(r: i32, g: i32, b: i32) -> u8 {
    clamp8((112 * r - 94 * g - 18 * b + 0x8080) >> 8)
}

/// Fill the Y/U/V planes (at their own strides) from tightly-packed `rgba` (w*h*4, R,G,B,A). `w` and `h`
/// are even. Chroma is the 2x2-averaged sample libyuv's ARGBToI420 produces.
fn rgba_to_i420(rgba: &[u8], w: usize, h: usize, y: &mut [u8], sy: usize, u: &mut [u8], su: usize, v: &mut [u8], sv: usize) {
    for j in 0..h {
        let row = &rgba[j * w * 4..];
        let yr = &mut y[j * sy..];
        for i in 0..w {
            let p = &row[i * 4..];
            yr[i] = rgb_to_y(p[0] as i32, p[1] as i32, p[2] as i32);
        }
    }
    for j in (0..h).step_by(2) {
        let (r0, r1) = (&rgba[j * w * 4..], &rgba[(j + 1) * w * 4..]);
        let ur = &mut u[(j / 2) * su..];
        let vr = &mut v[(j / 2) * sv..];
        for i in (0..w).step_by(2) {
            let (a, b2, c, d) = (&r0[i * 4..], &r0[(i + 1) * 4..], &r1[i * 4..], &r1[(i + 1) * 4..]);
            let r = (a[0] as i32 + b2[0] as i32 + c[0] as i32 + d[0] as i32 + 2) >> 2;
            let g = (a[1] as i32 + b2[1] as i32 + c[1] as i32 + d[1] as i32 + 2) >> 2;
            let b = (a[2] as i32 + b2[2] as i32 + c[2] as i32 + d[2] as i32 + 2) >> 2;
            ur[i / 2] = rgb_to_u(r, g, b);
            vr[i / 2] = rgb_to_v(r, g, b);
        }
    }
}

/// A libvpx VP8 encoder for one sending track, reused across frames (re-initing would re-key every frame).
pub(crate) struct Vp8Encoder {
    ctx: libvpx_sys::vpx_codec_ctx_t,
    w: usize,
    h: usize,
    fps: u32,
    pts: i64,
    first: bool,
    buf: Vec<u8>, // reused I420 staging, sized to the wrapped image's plane layout
}

impl Vp8Encoder {
    /// A libwebrtc-faithful realtime VP8 encoder at `w`x`h` (forced even), `fps`, `target_kbps`.
    pub(crate) fn new(w: usize, h: usize, fps: u32, target_kbps: u32) -> Option<Self> {
        let w = w & !1;
        let h = h & !1;
        if w == 0 || h == 0 {
            return None;
        }
        unsafe {
            let iface = libvpx_sys::vpx_codec_vp8_cx();
            if iface.is_null() {
                return None;
            }
            let mut cfg: libvpx_sys::vpx_codec_enc_cfg_t = std::mem::zeroed();
            if libvpx_sys::vpx_codec_enc_config_default(iface, &mut cfg, 0) != libvpx_sys::vpx_codec_err_t_VPX_CODEC_OK {
                return None;
            }
            cfg.g_w = w as c_uint;
            cfg.g_h = h as c_uint;
            cfg.g_timebase.num = 1;
            cfg.g_timebase.den = 90_000; // the RTP clock, so pts lines up with the packetizer's timestamp
            cfg.rc_target_bitrate = target_kbps;
            cfg.rc_end_usage = libvpx_sys::vpx_rc_mode_VPX_CBR;
            cfg.g_pass = libvpx_sys::vpx_enc_pass_VPX_RC_ONE_PASS;
            cfg.g_lag_in_frames = 0;
            cfg.g_error_resilient = libvpx_sys::VPX_ERROR_RESILIENT_DEFAULT;
            cfg.g_threads = 1;
            cfg.rc_min_quantizer = 2;
            cfg.rc_max_quantizer = 56;
            cfg.rc_undershoot_pct = 100;
            cfg.rc_overshoot_pct = 15;
            cfg.rc_buf_initial_sz = 500;
            cfg.rc_buf_optimal_sz = 600;
            cfg.rc_buf_sz = 1000;
            cfg.rc_dropframe_thresh = 0;
            cfg.kf_mode = libvpx_sys::vpx_kf_mode_VPX_KF_AUTO;
            cfg.kf_max_dist = 3000;

            let mut ctx: libvpx_sys::vpx_codec_ctx_t = std::mem::zeroed();
            if libvpx_sys::vpx_codec_enc_init_ver(&mut ctx, iface, &cfg, 0, libvpx_sys::VPX_ENCODER_ABI_VERSION as c_int) != libvpx_sys::vpx_codec_err_t_VPX_CODEC_OK {
                return None;
            }
            // libwebrtc's realtime desktop controls. The trailing variadic arg must be the exact C width:
            // CPUUSED / TOKEN_PARTITIONS are int, NOISE_SENSITIVITY / STATIC_THRESHOLD are unsigned int.
            libvpx_sys::vpx_codec_control_(&mut ctx, libvpx_sys::vp8e_enc_control_id_VP8E_SET_CPUUSED as c_int, -6 as c_int);
            libvpx_sys::vpx_codec_control_(&mut ctx, libvpx_sys::vp8e_enc_control_id_VP8E_SET_TOKEN_PARTITIONS as c_int, libvpx_sys::vp8e_token_partitions_VP8_ONE_TOKENPARTITION as c_int);
            libvpx_sys::vpx_codec_control_(&mut ctx, libvpx_sys::vp8e_enc_control_id_VP8E_SET_NOISE_SENSITIVITY as c_int, 0 as c_uint);
            libvpx_sys::vpx_codec_control_(&mut ctx, libvpx_sys::vp8e_enc_control_id_VP8E_SET_STATIC_THRESHOLD as c_int, 1 as c_uint);

            Some(Self { ctx, w, h, fps: fps.max(1), pts: 0, first: true, buf: Vec::new() })
        }
    }

    /// The (even) dimensions this encoder was created for, so the sender can recreate it on a resize.
    pub(crate) fn dims(&self) -> (usize, usize) {
        (self.w, self.h)
    }

    /// Encode one tightly-packed RGBA frame (`w*h*4`); returns the VP8 packets libvpx emits (usually one).
    /// `None` on an encode error; the caller treats an empty Vec (a dropped frame) as "send nothing".
    pub(crate) fn encode(&mut self, rgba: &[u8]) -> Option<Vec<EncodedFrame>> {
        let (w, h) = (self.w, self.h);
        if rgba.len() < w * h * 4 {
            return None;
        }
        unsafe {
            // Wrap a staging buffer as an I420 image; libvpx picks the plane strides (don't assume ==w).
            self.buf.resize(w * h * 2, 0); // ample for I420 (3*w*h/2) + any stride padding
            let mut img: libvpx_sys::vpx_image_t = std::mem::zeroed();
            if libvpx_sys::vpx_img_wrap(&mut img, libvpx_sys::vpx_img_fmt_VPX_IMG_FMT_I420, w as c_uint, h as c_uint, 1, self.buf.as_mut_ptr()).is_null() {
                return None;
            }
            let (sy, su, sv) = (img.stride[0] as usize, img.stride[1] as usize, img.stride[2] as usize);
            // The Y/U/V regions are disjoint within buf, so disjoint &mut slices over them are sound.
            let y = std::slice::from_raw_parts_mut(img.planes[0], sy * h);
            let u = std::slice::from_raw_parts_mut(img.planes[1], su * (h / 2));
            let v = std::slice::from_raw_parts_mut(img.planes[2], sv * (h / 2));
            rgba_to_i420(rgba, w, h, y, sy, u, su, v, sv);

            let flags: libvpx_sys::vpx_enc_frame_flags_t = if self.first { libvpx_sys::VPX_EFLAG_FORCE_KF as _ } else { 0 };
            let step = (90_000 / self.fps) as c_ulong;
            if libvpx_sys::vpx_codec_encode(&mut self.ctx, &img, self.pts, step, flags, libvpx_sys::VPX_DL_REALTIME as c_ulong) != libvpx_sys::vpx_codec_err_t_VPX_CODEC_OK {
                return None;
            }
            self.pts += step as i64;
            self.first = false;

            let mut out = Vec::new();
            let mut iter: libvpx_sys::vpx_codec_iter_t = std::ptr::null();
            loop {
                let pkt = libvpx_sys::vpx_codec_get_cx_data(&mut self.ctx, &mut iter);
                if pkt.is_null() {
                    break;
                }
                // VPX_CODEC_CX_FRAME_PKT == 0; the union's .frame is valid only for that kind, and its
                // buffer only until the next libvpx call, so copy it out now.
                if (*pkt).kind == 0 {
                    let frame = (*pkt).data.frame;
                    let bytes = std::slice::from_raw_parts(frame.buf as *const u8, frame.sz).to_vec();
                    let is_keyframe = frame.flags & libvpx_sys::VPX_FRAME_IS_KEY != 0;
                    out.push(EncodedFrame { data: bytes, is_keyframe });
                }
            }
            Some(out)
        }
    }
}

impl Drop for Vp8Encoder {
    fn drop(&mut self) {
        unsafe {
            libvpx_sys::vpx_codec_destroy(&mut self.ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_a_vp8_keyframe_then_interframes() {
        let (w, h) = (64usize, 48usize);
        let mut enc = Vp8Encoder::new(w, h, 30, 300).expect("encoder inits");
        // A solid green frame.
        let mut rgba = vec![0u8; w * h * 4];
        for px in rgba.chunks_mut(4) {
            px[1] = 200;
            px[3] = 255;
        }
        let first = enc.encode(&rgba).expect("encode ok");
        assert_eq!(first.len(), 1, "one VP8 packet");
        assert!(first[0].is_keyframe, "the first frame is forced to a keyframe");
        assert!(!first[0].data.is_empty(), "non-empty bitstream");
        // A VP8 keyframe carries the start code 0x9d 0x01 0x2a at bytes 3..6 (after the 3-byte frame tag),
        // and the tag's bit 0 (key-frame flag) is 0.
        let d = &first[0].data;
        assert!(d.len() > 10 && d[0] & 1 == 0, "key-frame tag");
        assert_eq!(&d[3..6], &[0x9d, 0x01, 0x2a], "VP8 keyframe start code");

        // A second, slightly different frame encodes as a (non-key) interframe.
        for px in rgba.chunks_mut(4) {
            px[2] = 120;
        }
        let second = enc.encode(&rgba).expect("encode ok");
        assert_eq!(second.len(), 1);
        assert!(!second[0].is_keyframe, "the second frame is an interframe");
        assert!(!second[0].data.is_empty());
    }
}
