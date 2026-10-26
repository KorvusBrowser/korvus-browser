//! `core/html/media_capabilities.rs` -- what Blink 150's `MediaCapabilities` decides about a CONVERTED configuration
//! before it answers (modules/media_capabilities/media_capabilities.cc), and its answer for a `webrtc` one
//! (platform/peerconnection/webrtc_{de,en}coding_info_handler.cc over libwebrtc's codec factories). The conversion
//! itself is WebIDL's (`install::ctor_dicts`); the binding is `media_objects::cb_capabilities_info`.
//!
//! Measured on stock Chrome 150.0.7871.46 (`tests/fixtures/dict-conversion/mediacaps-*`): a
//! `webrtc` question is answered from the device's RTP codecs -- VP8, VP9, H264 and AV1 decode and encode, H265 does
//! not; opus, G722, PCMU and PCMA do, `red`, `CN`, `telephone-event` and `rtx` are no codecs -- a video one never power
//! efficient (software), an audio one always; an encoder takes the scalability modes libwebrtc's encoder builds for its
//! codec. korvus answered every `webrtc` configuration unsupported.

use deno_core::v8;

use crate::binding::protos::get_prop;

// ── Blink's `ParsedContentType` (platform/network, `Mode::kNormal`) ─────────────────────────────────────────────

/// `HeaderFieldTokenizer`'s token characters in `Mode::kNormal` (printable ASCII but the separators).
fn token_char(c: char) -> bool {
    let u = c as u32;
    (0x20..128).contains(&u) && !matches!(c, ' ' | ';' | '"' | '(' | ')' | '<' | '>' | '@' | ',' | ':' | '\\' | '/' | '[' | ']' | '?' | '=')
}

struct Tokenizer {
    c: Vec<char>,
    i: usize,
}

impl Tokenizer {
    fn new(s: &str) -> Self {
        let mut t = Tokenizer { c: s.chars().collect(), i: 0 };
        t.ws();
        t
    }
    fn done(&self) -> bool {
        self.i >= self.c.len()
    }
    fn ws(&mut self) {
        while !self.done() && matches!(self.c[self.i], ' ' | '\t') {
            self.i += 1;
        }
    }
    fn consume(&mut self, ch: char) -> bool {
        if self.done() || self.c[self.i] != ch {
            return false;
        }
        self.i += 1;
        self.ws();
        true
    }
    fn token(&mut self) -> Option<String> {
        let start = self.i;
        while !self.done() && token_char(self.c[self.i]) {
            self.i += 1;
        }
        if start == self.i {
            return None;
        }
        let t = self.c[start..self.i].iter().collect();
        self.ws();
        Some(t)
    }
    fn quoted(&mut self) -> Option<String> {
        self.i += 1;
        let mut out = String::new();
        while !self.done() {
            match self.c[self.i] {
                '"' => {
                    self.i += 1;
                    self.ws();
                    return Some(out);
                }
                '\\' => {
                    self.i += 1;
                    if self.done() {
                        return None;
                    }
                }
                _ => {}
            }
            out.push(self.c[self.i]);
            self.i += 1;
        }
        None
    }
    fn token_or_quoted(&mut self) -> Option<String> {
        if self.done() {
            return None;
        }
        if self.c[self.i] == '"' {
            return self.quoted();
        }
        self.token()
    }
}

/// `type/subtype` (its case kept) and the `name=value` parameters, or `None` when the string does not parse.
pub(crate) fn parsed_content_type(s: &str) -> Option<(String, Vec<(String, String)>)> {
    let mut t = Tokenizer::new(s);
    let ty = t.token()?;
    if !t.consume('/') {
        return None;
    }
    let sub = t.token()?;
    let mut params = Vec::new();
    while !t.done() {
        if !t.consume(';') {
            return None;
        }
        let k = t.token()?;
        if !t.consume('=') {
            return None;
        }
        params.push((k, t.token_or_quoted()?));
    }
    Some((format!("{ty}/{sub}"), params))
}

/// `IsValidMimeType`: a MIME type under `prefix` (or `application/`, outside WebRTC), and outside WebRTC at most one
/// parameter, `codecs`.
fn valid_mime(content_type: &str, prefix: &str, webrtc: bool) -> bool {
    let Some((mime, params)) = parsed_content_type(content_type) else { return false };
    if !mime.starts_with(prefix) && (webrtc || !mime.starts_with("application/")) {
        return false;
    }
    if webrtc {
        return true;
    }
    match params.len() {
        0 => true,
        1 => params[0].0.eq_ignore_ascii_case("codecs"),
        _ => false,
    }
}

fn member_str(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, k: &str) -> Option<String> {
    get_prop(scope, o, k).filter(|v| v.is_string()).map(|v| v.to_rust_string_lossy(scope))
}

fn has_member(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, k: &str) -> bool {
    get_prop(scope, o, k).is_some_and(|v| !v.is_undefined())
}

/// `IsValidMediaDecodingConfiguration` / `IsValidMediaEncodingConfiguration` over the converted dictionary: the
/// sentence Blink rejects with, or `None`.
pub(crate) fn invalid_configuration(
    scope: &mut v8::PinScope,
    config: v8::Local<v8::Object>,
    video: Option<v8::Local<v8::Object>>,
    audio: Option<v8::Local<v8::Object>>,
    decoding: bool,
    webrtc: bool,
) -> Option<&'static str> {
    if video.is_none() && audio.is_none() {
        return Some("The configuration dictionary has neither |video| nor |audio| specified and needs at least one of them.");
    }
    if decoding {
        if let Some(ksc) = get_prop(scope, config, "keySystemConfiguration").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
            if webrtc {
                return Some("The keySystemConfiguration object cannot be set for webrtc MediaDecodingType.");
            }
            if has_member(scope, ksc, "audio") && audio.is_none() {
                return Some("The keySystemConfiguration object contains an audio property but the root configuration has no audio configuration.");
            }
            if has_member(scope, ksc, "video") && video.is_none() {
                return Some("The keySystemConfiguration object contains a video property but the root configuration has no video configuration.");
            }
        }
    }
    if let Some(v) = video {
        let content_type = member_str(scope, v, "contentType").unwrap_or_default();
        let framerate = get_prop(scope, v, "framerate").and_then(|f| f.number_value(scope)).unwrap_or(f64::NAN);
        // scalabilityMode is a WebRTC encoder's question, spatialScalability a WebRTC decoder's
        let valid = valid_mime(&content_type, "video/", webrtc)
            && framerate.is_finite()
            && framerate > 0.0
            && !(has_member(scope, v, "scalabilityMode") && (!webrtc || decoding))
            && !(has_member(scope, v, "spatialScalability") && (!webrtc || !decoding));
        if !valid {
            return Some("The video configuration dictionary is not valid.");
        }
    }
    if let Some(a) = audio {
        let content_type = member_str(scope, a, "contentType").unwrap_or_default();
        if !valid_mime(&content_type, "audio/", webrtc) {
            return Some("The audio configuration dictionary is not valid.");
        }
    }
    None
}

// ── a `webrtc` configuration: the device's RTP codecs ───────────────────────────────────────────────────────────

/// RTP payloads the media engine lists beside the codecs that no codec factory makes.
const NOT_CODECS: [&str; 7] = ["rtx", "red", "ulpfec", "flexfec-03", "cn", "telephone-event", "comfort-noise"];

/// The codecs of `side` (`RTCRtpReceiver` / `RTCRtpSender`) and `kind` the worn device lists, each as its name (the
/// MIME subtype) and its fmtp parameters.
fn device_codecs(scope: &mut v8::PinScope, side: &str, kind: &str) -> Vec<(String, Vec<(String, String)>)> {
    let text = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let state = op_state.borrow();
        crate::binding::protos::persona_handle(&state)
            .and_then(|h| h.0.webrtc_capabilities())
            .and_then(|c| c.by_side_kind.get(&format!("{side}.{kind}")).cloned())
    };
    let Some(caps) = text.and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()) else { return Vec::new() };
    let prefix = format!("{kind}/");
    caps.get("codecs")
        .and_then(|c| c.as_array())
        .map(|codecs| {
            codecs
                .iter()
                .filter_map(|c| {
                    let mime = c.get("mimeType")?.as_str()?;
                    let name = mime.get(prefix.len()..).filter(|_| mime.len() > prefix.len() && mime[..prefix.len()].eq_ignore_ascii_case(&prefix))?;
                    let fmtp = c.get("sdpFmtpLine").and_then(|f| f.as_str()).unwrap_or("");
                    let params = fmtp
                        .split(';')
                        .filter_map(|p| p.split_once('=').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
                        .collect();
                    Some((name.to_string(), params))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn param<'a>(params: &'a [(String, String)], k: &str) -> Option<&'a str> {
    params.iter().rev().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
}

/// libwebrtc's H.264 profile of `profile-level-id` (`ParseSdpForH264ProfileLevelId`; absent: Constrained Baseline).
/// `None` for an id it does not recognise.
fn h264_profile(params: &[(String, String)]) -> Option<u8> {
    let Some(id) = param(params, "profile-level-id") else { return Some(0) };
    if id.len() != 6 {
        return None;
    }
    // strtol: the leading hex digits
    let digits: String = id.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
    let n = u32::from_str_radix(&digits, 16).unwrap_or(0);
    if n == 0 {
        return None;
    }
    let (level, iop, idc) = (n & 0xFF, (n >> 8) & 0xFF, (n >> 16) & 0xFF);
    // the level_idc values libwebrtc parses (1.1 with constraint_set3 is 1b)
    if !matches!(level, 10..=13 | 20..=22 | 30..=32 | 40..=42 | 50..=52) {
        return None;
    }
    // (profile_idc, profile_iop pattern -- x: either bit, then the profile): kProfilePatterns
    const PATTERNS: [(u32, &str, u8); 9] = [
        (0x42, "x1xx0000", 0),
        (0x4D, "1xxx0000", 0),
        (0x58, "11xx0000", 0),
        (0x42, "x0xx0000", 1),
        (0x58, "10xx0000", 1),
        (0x4D, "0x0x0000", 2),
        (0x64, "00000000", 3),
        (0x64, "00001100", 4),
        (0xF4, "00000000", 5),
    ];
    PATTERNS.iter().find(|(pidc, pat, _)| {
        *pidc == idc
            && pat.chars().enumerate().all(|(i, b)| {
                let bit = (iop >> (7 - i)) & 1;
                b == 'x' || (b == '1') == (bit == 1)
            })
    })
    .map(|(_, _, p)| *p)
}

/// libwebrtc's `IsSameCodec` for two formats of one name: VP9 and AV1 by profile, every other codec but H.264 by its
/// name alone.
fn same_codec(name: &str, a: &[(String, String)], b: &[(String, String)]) -> bool {
    let small_profile = |p: &[(String, String)], k: &str, max: i32| -> Option<i32> {
        match param(p, k) {
            None => Some(0),
            Some(v) => v.parse::<i32>().ok().filter(|n| (0..=max).contains(n)),
        }
    };
    match name.to_ascii_lowercase().as_str() {
        "vp9" => small_profile(a, "profile-id", 3).is_some() && small_profile(a, "profile-id", 3) == small_profile(b, "profile-id", 3),
        "av1" => small_profile(a, "profile", 2).is_some() && small_profile(a, "profile", 2) == small_profile(b, "profile", 2),
        _ => true,
    }
}

/// `H264IsProfileSubsetOf` (ITU-T H.264 Annex A.2), the profiles as [`h264_profile`] numbers them: Constrained
/// Baseline 0, Baseline 1, Main 2, High 3, Constrained High 4, Predictive High 4:4:4 5.
fn h264_profile_subset(sub: u8, sup: u8) -> bool {
    sub == sup
        || match sup {
            1 | 2 | 4 => sub == 0,
            3 => matches!(sub, 0 | 2 | 4),
            5 => matches!(sub, 0 | 2 | 3 | 4),
            _ => false,
        }
}

/// The software decoder factory's answer (`InternalDecoderFactory::QueryCodecSupport`): H.264 when one of its
/// formats has the same packetization mode and a profile the stream's is a subset of, any other codec when a format
/// is the same codec.
fn decoder_takes(formats: &[(String, Vec<(String, String)>)], name: &str, params: &[(String, String)]) -> bool {
    if name.eq_ignore_ascii_case("h264") {
        let mode = |p: &[(String, String)]| param(p, "packetization-mode").unwrap_or("0").to_string();
        let Some(profile) = h264_profile(params) else { return false };
        return formats.iter().any(|(n, p)| {
            n.eq_ignore_ascii_case("h264") && mode(p) == mode(params) && h264_profile(p).is_some_and(|sup| h264_profile_subset(profile, sup))
        });
    }
    formats.iter().any(|(n, p)| n.eq_ignore_ascii_case(name) && same_codec(name, params, p))
}

/// The codec profile `WebRtcVideoFormatToMediaVideoCodecProfile` maps a format to is known: VP8 and AV1 always,
/// VP9 and H.264 when their profile parses.
fn known_codec_profile(name: &str, params: &[(String, String)]) -> bool {
    match name.to_ascii_lowercase().as_str() {
        "vp8" | "av1" => true,
        "vp9" => param(params, "profile-id").is_none_or(|v| v.parse::<i32>().is_ok_and(|n| (0..=3).contains(&n))),
        "h264" => h264_profile(params).is_some(),
        _ => false,
    }
}

/// The scalability modes libwebrtc's encoder for `name` builds: VP8's and OpenH264's temporal layers, and every mode
/// `ScalabilityStructureConfig` knows for VP9 and AV1.
fn encoder_takes_mode(name: &str, mode: &str) -> bool {
    const TEMPORAL: [&str; 3] = ["L1T1", "L1T2", "L1T3"];
    const SVC: [&str; 34] = [
        "L1T1", "L1T2", "L1T3", "L2T1", "L2T1_KEY", "L2T1h", "L2T2", "L2T2_KEY", "L2T2_KEY_SHIFT", "L2T2h", "L2T3", "L2T3_KEY", "L2T3h",
        "L3T1", "L3T1_KEY", "L3T1h", "L3T2", "L3T2_KEY", "L3T2h", "L3T3", "L3T3_KEY", "L3T3h", "S2T1", "S2T1h", "S2T2", "S2T2h", "S2T3",
        "S2T3h", "S3T1", "S3T1h", "S3T2", "S3T2h", "S3T3", "S3T3h",
    ];
    match name.to_ascii_lowercase().as_str() {
        "vp8" | "h264" => TEMPORAL.contains(&mode),
        "vp9" | "av1" => SVC.contains(&mode),
        _ => false,
    }
}

/// `(supported, smooth, powerEfficient)` of a valid `webrtc` configuration (`WebrtcDecodingInfoHandler` /
/// `WebrtcEncodingInfoHandler`, then `MediaCapabilities::OnWebrtcSupportInfo`): the audio first -- always power
/// efficient when supported -- and the video only when the audio is supported: a decoder's from the software factory's
/// formats (the receiver's codecs; reference scaling VP9's and AV1's alone), an encoder's the first format of the
/// codec's name (`FuzzyMatchSdpVideoFormat`) and its scalability modes; never power efficient (software). `smooth`
/// is `supported` unless a supported video of some pixels asks the perf history, which has no data on a fresh
/// profile and answers smooth -- but not for an unknown codec profile, more than 10 x 4K pixels, or a frame rate
/// rounding outside 1-1000.
pub(crate) fn webrtc_support(scope: &mut v8::PinScope, video: Option<v8::Local<v8::Object>>, audio: Option<v8::Local<v8::Object>>, decoding: bool) -> (bool, bool, bool) {
    let side = if decoding { "RTCRtpReceiver" } else { "RTCRtpSender" };
    let (mut supported, mut power_efficient) = (true, true);
    if let Some(a) = audio {
        let ct = member_str(scope, a, "contentType").unwrap_or_default();
        let name = parsed_content_type(&ct).and_then(|(m, _)| m.get("audio/".len()..).map(str::to_ascii_lowercase)).unwrap_or_default();
        supported = !NOT_CODECS.contains(&name.as_str()) && device_codecs(scope, side, "audio").iter().any(|(n, _)| n.eq_ignore_ascii_case(&name));
        power_efficient = supported;
    }
    let Some(v) = video else { return (supported, supported, power_efficient) };
    if !supported {
        return (false, false, power_efficient);
    }
    let ct = member_str(scope, v, "contentType").unwrap_or_default();
    let Some((mime, params)) = parsed_content_type(&ct) else { return (false, false, false) };
    let name = mime.get("video/".len()..).unwrap_or_default().to_string();
    let lower = name.to_ascii_lowercase();
    let formats: Vec<_> = device_codecs(scope, side, "video").into_iter().filter(|(n, _)| !NOT_CODECS.contains(&n.to_ascii_lowercase().as_str())).collect();
    let supported = if decoding {
        let spatial = get_prop(scope, v, "spatialScalability").is_some_and(|s| s.is_true());
        (!spatial || lower == "vp9" || lower == "av1") && decoder_takes(&formats, &name, &params)
    } else {
        let mode = member_str(scope, v, "scalabilityMode");
        formats.iter().any(|(n, _)| n.eq_ignore_ascii_case(&name)) && mode.is_none_or(|m| encoder_takes_mode(&name, &m))
    };
    let number = |scope: &mut v8::PinScope, k: &str| get_prop(scope, v, k).and_then(|n| n.number_value(scope)).unwrap_or(0.0);
    let pixels = number(scope, "width") * number(scope, "height");
    let fps = number(scope, "framerate").round();
    let smooth = supported && (pixels == 0.0 || (known_codec_profile(&name, &params) && pixels <= (10 * 3840 * 2160) as f64 && (1.0..=1000.0).contains(&fps)));
    (supported, smooth, false)
}
