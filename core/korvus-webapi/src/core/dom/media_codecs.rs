//! `core/dom/media_codecs.rs` -- "can you play this?", for the three members that answer it.
//!
//! The GRAMMAR that reads [`spec::CodecGrid`] -- the recorded container/codec answer grid of the
//! device this realm is WEARING, read off the active bundle by `persona::codecs`. It was
//! one compile-time constant, `generated_media_codecs::MEDIA_CONTAINERS_JSON`, derived from the
//! shipped bundle and only the shipped bundle; an engine wearing Opera 135 therefore claimed the
//! proprietary H.264/AAC decoders that Opera's Linux build does not have. Measured over the four
//! captures in this tree: 274 answers wrong under Opera, 16 under Edge, 0 under Chrome.
//!
//! **THE GRID IS DATA AND THIS FILE IS THE ALGORITHM**, and that separation is the reason the fix is
//! a seam and not a table swap. The bundle says what the device answered for the 37 containers and 60
//! codec spellings the sweep asked about; everything below -- the MIME parser, the `avc1.PPCCLL` /
//! `vp09.PP.LL.DD` / `mp4a.40.N` / `av01.P.LLT.DD` parameter grammars, and the three DIFFERENT
//! questions the three members ask -- is this file's, identical under every device.
//!
//! `dom/00c-media-codecs.js` (366 lines) was this file, and handed its three
//! answers to JS through the bridge slots `__codecCanPlayType`, `__codecMediaSourceSupported` and
//! `__codecRecorderSupported`. All three are gone, and so is every JS interface that read them:
//! `HTMLMediaElement.canPlayType` is a Rust row of `core/html/html_media_element.rs`, and
//! `MediaSource.isTypeSupported` / `MediaRecorder.isTypeSupported` are Rust rows of
//! `core/media/media_source.rs` and `core/media/media_recorder.rs` that call [`media_source_supported`]
//! and [`recorder_supported`] below. One place computes the answer for all three.
//!
//! WHY THIS IS NOT A LOOKUP TABLE WEARING A HAT. The recorded grid asked about 57 codec spellings; a
//! page can send any string at all. So the table is reduced to CANONICAL CODEC IDS by the parser
//! below -- `avc1.42E01E`, `avc1.4D401F` and `avc1.640028` are all the id `avc1`, reached by parsing
//! the profile/constraint/level triplet and rejecting it when it does not parse. A spelling the sweep
//! never asked about (`avc1.4D4028`, `vp09.01.20.10`, `mp4a.40.23`) therefore gets a COMPUTED answer,
//! and a bug in the grammar produces a wrong one. That is the property a stub does not have.
//!
//! THE THREE MEMBERS DO NOT AGREE, and the difference is recorded, not invented:
//!
//!   * `canPlayType` and `MediaSource.isTypeSupported` validate the codec's parameters. `avc1` on its
//!     own, `avc1.42E0` and `avc1.ZZZZZZ` are all rejected.
//!   * `MediaRecorder.isTypeSupported` matches the codec FAMILY case-insensitively for a container
//!     whose muxer never parses the parameters, and reads the whole string for one that does:
//!     `avc1`, `avc1.ZZZZZZ` and `AVC1.42E01E` are all accepted for `video/webm`, and
//!     `vp09.00.10.08` is not (its family is `vp09`, and MediaRecorder's WebM list has `vp9`).
//!   * The containers differ per member. `video/mp4` with no codecs is `"maybe"` to `canPlayType`,
//!     `false` to MediaSource (which requires codecs) and `true` to MediaRecorder. `video/mp2t` is
//!     `""` to `canPlayType` and a supported MSE container.
//!   * `MediaRecorder.isTypeSupported("")` is TRUE -- the empty type means "choose a default".
//!
//! Codec ids are CASE-SENSITIVE for canPlayType/MSE and case-insensitive for MediaRecorder, which is
//! recorded rather than assumed: `Opus` is accepted by all three (Chrome's codec map carries both
//! spellings, because that is how WebM wrote it), while `VP8` is accepted only by MediaRecorder.
//!
//! **`canPlayType` AND `MediaSource.isTypeSupported` RUN CHROMIUM'S OWN ALGORITHM NOW (PHASE 3 F7).** The
//! grid answered only the 37 containers it was swept over, so every container the sweep never asked about
//! was `""` -- `audio/x-m4a`, `video/x-m4v`, `audio/matroska`, `video/matroska` and `audio/x-mpegurl`, all
//! `"maybe"` in Chrome 150 (a device-check script's `acma` signal reads the first). Chromium's `MimeUtil`
//! (media/base/mime_util_internal.cc) is the algorithm -- its container table
//! (`AddSupportedMediaFormats`), its codec-string parsers (`ParseCodecHelper` and
//! media/base/video_codec_string_parsers.cc), its ambiguity rules (`AreSupportedCodecs`) and its platform
//! check (`IsCodecSupported`) -- and Blink's glue around it (`HTMLMediaElement::GetSupportsType`,
//! `MediaSource::IsTypeSupportedInternal`, media/filters/stream_parser_factory.cc). The worn device supplies
//! only what is the device's ([`Device`]): which decoders it has, which build flags its vendor shipped, and a
//! container table its vendor patched. Checked before the port against all four bundles' 27,312
//! `canPlayType`/MSE answers and 4,904 answers recorded off stock Chrome 150: no difference.
//! `MediaRecorder` and `MediaCapabilities` still answer from the reduced table below.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use deno_core::v8;

// ---- the MIME string ----------------------------------------------------------------------------

/// A parsed `type/subtype; codecs="a, b"`.
struct Mime {
    /// The lower-cased `type/subtype`.
    ty: String,
    /// The `codecs` parameter's list, or `None` when the parameter was absent or carried nothing.
    codecs: Option<Vec<String>>,
}

/// Split `type/subtype; codecs="a, b"` the way Chrome's content-type parser does.
///
/// Recorded behaviour this reproduces: the type is lower-cased (`VIDEO/WEBM` is `video/webm`), the
/// parameter NAME is matched case-insensitively, the value may be quoted or bare, surrounding
/// whitespace is dropped from the type, from each parameter and from each codec, parameters other
/// than `codecs` are ignored, a repeated `codecs` keeps the FIRST, and both `codecs=""` and a bare
/// `codecs` with no value mean "no codecs given" rather than "an empty codec".
///
/// Returns `None` when there is no `type/subtype` at all.
fn parse_mime(input: &str) -> Option<Mime> {
    let semi = input.find(';');
    let head = match semi {
        Some(i) => &input[..i],
        None => input,
    }
    .trim();
    if !head.contains('/') {
        return None;
    }
    let ty = head.to_ascii_lowercase();
    let mut codecs: Option<Vec<String>> = None;
    if let Some(i) = semi {
        for part in input[i + 1..].split(';') {
            let Some(eq) = part.find('=') else { continue };
            let name = part[..eq].trim().to_ascii_lowercase();
            if name != "codecs" || codecs.is_some() {
                continue;
            }
            let mut v = part[eq + 1..].trim();
            if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
                v = &v[1..v.len() - 1];
            }
            let list: Vec<String> = v.split(',').map(|c| c.trim()).filter(|c| !c.is_empty()).map(|c| c.to_string()).collect();
            codecs = Some(list);
        }
    }
    // A `codecs` parameter that carried nothing is the same as no parameter: recorded as `"maybe"`
    // for `video/webm; codecs=""`, not as an unknown codec.
    if codecs.as_ref().is_some_and(|l| l.is_empty()) {
        codecs = None;
    }
    Some(Mime { ty, codecs })
}

// ---- the codec grammar --------------------------------------------------------------------------

/// H.264 profile_idc values Chrome decodes: baseline (0x42), main (0x4D), extended (0x58) and
/// high (0x64). The high-bit-depth profiles parse and are not decodable on this build, which is why
/// `avc1.6E00xx` is not in the recorded grid's accepted set.
const H264_PROFILES: &[u32] = &[0x42, 0x4d, 0x58, 0x64];
/// level_idc, as the spec numbers them: 1, 1.1 ... 6.2. Anything else is not a level.
const H264_LEVELS: &[u32] = &[10, 11, 12, 13, 20, 21, 22, 30, 31, 32, 40, 41, 42, 50, 51, 52, 60, 61, 62];
/// The MPEG-4 audio object types Chrome's AAC decoder accepts: AAC-LC, HE-AAC (SBR) and HE-AACv2 (PS).
const AAC_OBJECT_TYPES: &[u32] = &[2, 5, 29];
/// VP9 profiles and the bit depths each may declare.
const VP9_PROFILES: &[u32] = &[0, 1, 2, 3];
const VP9_BIT_DEPTHS: &[u32] = &[8, 10, 12];

/// The families whose spelling carries parameters, and so can be generalised beyond the recording.
const PARAMETERISED: &[&str] = &["avc1", "avc3", "vp09", "av01", "mp4a", "hev1", "hvc1"];

/// The canonical id a codec string denotes.
struct Codec {
    id: String,
    /// A codec that names a family but not enough of it to decide: `avc1`, `avc3`, `mp4a.40`.
    ambiguous: bool,
    /// The `vp8.0` / `vp9.0` spellings: the same codec to `canPlayType`, not a codec to MSE.
    legacy: bool,
}

fn codec(id: &str) -> Option<Codec> {
    Some(Codec { id: id.to_string(), ambiguous: false, legacy: false })
}
fn codec_ambiguous(id: &str) -> Option<Codec> {
    Some(Codec { id: id.to_string(), ambiguous: true, legacy: false })
}
fn codec_legacy(id: &str) -> Option<Codec> {
    Some(Codec { id: id.to_string(), ambiguous: false, legacy: true })
}

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit())
}
fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// The canonical id a codec string denotes, or `None` when the string is not a codec at all.
///
/// Three outcomes, not two, because the recording has three. `video/mp4; codecs="avc1.42E01E"` is
/// `"probably"`, `video/mp4; codecs="nonsense"` is `""`, and `video/mp4; codecs="avc1"` is `"maybe"`
/// -- a bare `avc1` names H.264 without saying which profile and level, so the browser knows the
/// container and the family and still cannot promise. `avc3` and `mp4a.40` behave the same way, and
/// no other spelling in the sweep does.
///
/// The id is what the recorded per-container sets are reduced to, so it has to be fine-grained enough
/// to keep every distinction the recording makes and no finer. `mp4a` is the one that forces the
/// shape: `video/mp4` accepts object types 0x40 (AAC), 0x66, 0x67, 0x69 and 0x6B, while MSE's
/// `video/mp4` accepts only 0x40 and 0x67 -- so the object type has to survive into the id, and
/// `mp4a#103` is that id.
fn canonical_codec(raw: &str) -> Option<Codec> {
    if raw.is_empty() {
        return None;
    }
    let dot = raw.find('.');
    let (family, rest) = match dot {
        Some(i) => (&raw[..i], &raw[i + 1..]),
        None => (raw, ""),
    };
    match family {
        "avc1" | "avc3" => {
            if dot.is_none() {
                return codec_ambiguous(family);
            }
            if rest.len() != 6 || !is_hex(rest) {
                return None;
            }
            let profile = u32::from_str_radix(&rest[0..2], 16).ok()?;
            let level = u32::from_str_radix(&rest[4..6], 16).ok()?;
            if !H264_PROFILES.contains(&profile) || !H264_LEVELS.contains(&level) {
                return None;
            }
            codec(family)
        }
        "hev1" | "hvc1" => {
            // `hvc1.<profile>.<compat flags>.<tier><level>[.<constraints>...]`, e.g. hvc1.1.6.L93.B0.
            // The headed capture has a hardware HEVC decoder and answers "probably" for the full form
            // and "" for a bare `hev1`, the same way `vp09` needs its fields: HEVC has no "maybe"
            // spelling.
            let f: Vec<&str> = if dot.is_none() { Vec::new() } else { rest.split('.').collect() };
            if f.len() < 3 {
                return None;
            }
            // `[ABC]?[0-9]{1,2}`
            let p0 = f[0];
            let p0b = if p0.starts_with(['A', 'B', 'C']) { &p0[1..] } else { p0 };
            if p0b.is_empty() || p0b.len() > 2 || !is_digits(p0b) {
                return None;
            }
            // `[0-9A-Fa-f]{1,8}`
            if f[1].len() > 8 || !is_hex(f[1]) {
                return None;
            }
            // `[LH][0-9]{1,3}`
            let p2 = f[2];
            if !p2.starts_with(['L', 'H']) {
                return None;
            }
            let p2b = &p2[1..];
            if p2b.is_empty() || p2b.len() > 3 || !is_digits(p2b) {
                return None;
            }
            for c in &f[3..] {
                if c.len() > 2 || !is_hex(c) {
                    return None;
                }
            }
            codec(family)
        }
        "vp8" | "vp9" => {
            // `vp8` and the legacy `vp8.0` spelling name the same codec to `canPlayType`, which
            // answers "probably" for both. They are NOT the same to MediaSource, which answers true
            // for `vp8` and false for `vp8.0`, so the alias is tagged rather than collapsed.
            if dot.is_none() {
                return codec(family);
            }
            if rest == "0" { codec_legacy(family) } else { None }
        }
        "vp09" => {
            // `vp09.<profile>.<level>.<bitDepth>[...]`, two digits each, with the optional colour
            // fields after it. Three fields is the minimum; `vp09.00.10` is rejected by the recording.
            let f: Vec<&str> = rest.split('.').collect();
            if f.len() < 3 {
                return None;
            }
            for x in &f {
                if x.len() != 2 || !is_digits(x) {
                    return None;
                }
            }
            let profile: u32 = f[0].parse().ok()?;
            let depth: u32 = f[2].parse().ok()?;
            if !VP9_PROFILES.contains(&profile) || !VP9_BIT_DEPTHS.contains(&depth) {
                return None;
            }
            codec("vp09")
        }
        "av01" => {
            // `av01.<profile>.<level><tier>.<bitDepth>[...]`, e.g. av01.0.04M.08.
            let f: Vec<&str> = rest.split('.').collect();
            if f.len() < 3 {
                return None;
            }
            if !is_digits(f[0]) {
                return None;
            }
            // `[0-9]{2}[MH]`
            if f[1].len() != 3 || !is_digits(&f[1][0..2]) || !f[1][2..].starts_with(['M', 'H']) {
                return None;
            }
            if f[2].len() != 2 || !is_digits(f[2]) {
                return None;
            }
            codec("av01")
        }
        "av1" => {
            if dot.is_none() { codec("av1") } else { None }
        }
        "mp4a" => {
            // `mp4a.<objectType>` in hex, and for AAC (0x40) a third decimal field naming the audio
            // object type. `mp4a` alone is not a codec, `mp4a.40` alone is ambiguous, and `mp4a.40.34`
            // (MP3 carried as an AAC object type) is a codec this build does not decode.
            if dot.is_none() {
                return None;
            }
            let f: Vec<&str> = rest.split('.').collect();
            if f[0].len() != 2 || !is_hex(f[0]) {
                return None;
            }
            let oti = u32::from_str_radix(f[0], 16).ok()?;
            if oti == 0x40 {
                if f.len() == 1 {
                    return codec_ambiguous(&format!("mp4a#{oti}"));
                }
                if f.len() != 2 || !is_digits(f[1]) {
                    return None;
                }
                let aot: u32 = f[1].parse().ok()?;
                if !AAC_OBJECT_TYPES.contains(&aot) {
                    return None;
                }
            } else if f.len() != 1 {
                return None;
            }
            codec(&format!("mp4a#{oti}"))
        }
        // A plain name -- `opus`, `Opus`, `vorbis`, `flac`, `mp3`, `1` -- is its own id, kept exactly
        // as written because the recording distinguishes `Opus` from `OPUS`. Anything carrying a dot
        // under an unknown family is not a codec.
        _ => {
            if dot.is_none() { codec(raw) } else { None }
        }
    }
}

/// `type/subtype` and how many codecs the `codecs` parameter names, or `None` when the string is not
/// a content type at all. `MediaCapabilities` needs both halves before it may ask the grid anything:
/// a configuration whose `contentType` does not parse is a TypeError rather than an
/// unsupported answer, and one that names more than a single codec is unsupported rather than an
/// error -- measured, both, on Brave 151 and Chrome 150 alike.
pub fn content_type_shape(input: &str) -> Option<(String, usize)> {
    let m = parse_mime(input)?;
    Some((m.ty, m.codecs.as_ref().map(|c| c.len()).unwrap_or(0)))
}

/// The family half of a codec string: everything before the first `.`.
fn family_of(raw: &str) -> &str {
    match raw.find('.') {
        Some(i) => &raw[..i],
        None => raw,
    }
}

// ---- the recorded table, reduced to ids ---------------------------------------------------------

struct Row {
    /// `base.video`: `canPlayType`'s answer for the container with no codecs given.
    base_video: String,
    base_ms: bool,
    base_mr: bool,
    /// The recorded `codecs.video` / `codecs.ms` sets, reduced to canonical ids.
    ids_video: HashSet<String>,
    ids_ms: HashSet<String>,
    /// MediaRecorder keeps the recorded SPELLINGS, because its two muxers read different amounts of
    /// them -- see [`recorder_lenient`].
    mr_lenient: bool,
    mr_exact: HashSet<String>,
    mr_family: HashSet<String>,
    /// Spellings a LENIENT container rejected whose family it otherwise accepts (`spec::CodecContainer::mr_reject`):
    /// the lenient family match would wrongly accept them, so they are denied first. Empty for an exact container.
    mr_deny: HashSet<String>,
}

/// MediaRecorder does not ask the question `canPlayType` asks, and does not answer it the same way.
/// It is muxing, not decoding, so what it checks is whether its muxer for THIS container has a track
/// for the codec -- and the two muxers disagree about how much of the codec string they read.
///
/// The WebM/Matroska path matches the family and stops: `avc1`, `avc1.ZZZZZZ`, `AVC1.42E01E` and
/// `VP8` are all accepted for `video/webm`. The MP4 path reads the whole string: `avc1` is accepted,
/// `avc1.42E0` is not, `Opus` is not, and `vp9.0` is not while `vp9` is.
///
/// Which container is which is not asserted here -- it is READ OFF the recording. A container whose
/// recorded accepted set contains a parameterised spelling that does not parse (`avc1.ZZZZZZ`) is one
/// whose muxer never looked, and that is the whole classification.
fn recorder_lenient(mr: &[String]) -> bool {
    for c in mr {
        if c.is_empty() {
            continue;
        }
        let fam = family_of(c);
        if fam != c.as_str() && PARAMETERISED.contains(&fam) && canonical_codec(c).is_none() {
            return true;
        }
    }
    false
}

/// One device's recorded grid, reduced to the form the three questions are answered from.
type Table = HashMap<String, Row>;

/// THE ACTIVE DEVICE'S QUERY TABLE, memoised by capture id.
///
/// This reduction is the GRAMMAR's, not the bundle's: it canonicalises each recorded spelling through
/// [`canonical_codec`] and classifies each container's MediaRecorder behaviour through
/// [`recorder_lenient`], both of which are parser facts. The bundle supplies the answers
/// ([`spec::CodecGrid`], derived once per process by `persona::codecs`); this supplies what they mean.
/// Two layers of memo, because they are keyed by different things — the grid by the bundle it is read
/// from, the table by the grid it reduces.
fn table_for(grid: &spec::CodecGrid) -> Arc<Table> {
    static TABLES: OnceLock<Mutex<HashMap<String, Arc<Table>>>> = OnceLock::new();
    let tables = TABLES.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(t) = tables.lock() {
        if let Some(hit) = t.get(&grid.capture_id) {
            return hit.clone();
        }
    }
    // The recorded sets hold what answered "probably"/true, so nothing in them is ambiguous; an
    // ambiguous spelling reaches the set only through a query.
    // DIAGNOSTIC, analysis builds only: KORVUS_DIAG_NO_HEVC=1 answers the worn device's grid
    // without HEVC -- a GPU-less Chrome's (hardware is HEVC's only decoder), to tell whether a verdict follows it
    #[cfg(feature = "analysis")]
    let no_hevc = std::env::var_os("KORVUS_DIAG_NO_HEVC").is_some_and(|v| v == "1");
    #[cfg(not(feature = "analysis"))]
    let no_hevc = false;
    let reduce = |list: &[String]| -> HashSet<String> {
        list.iter()
            .filter_map(|c| canonical_codec(c))
            .filter(|k| !k.ambiguous && !(no_hevc && matches!(k.id.as_str(), "hev1" | "hvc1")))
            .map(|k| k.id)
            .collect()
    };
    let mut out = Table::with_capacity(grid.containers.len());
    for (container, c) in &grid.containers {
        let mut mr_exact = HashSet::new();
        let mut mr_family = HashSet::new();
        for k in &c.mr {
            if k.is_empty() {
                continue;
            }
            mr_exact.insert(k.clone());
            mr_family.insert(family_of(k).to_ascii_lowercase());
        }
        // Lower-cased, like `mr_family`: the lenient path matches case-insensitively, so a denial must too.
        let mr_deny: HashSet<String> = c.mr_reject.iter().map(|k| k.to_ascii_lowercase()).collect();
        out.insert(
            container.clone(),
            Row {
                base_video: c.base_video.clone(),
                base_ms: c.base_ms,
                base_mr: c.base_mr,
                ids_video: reduce(&c.video),
                ids_ms: reduce(&c.ms),
                mr_lenient: recorder_lenient(&c.mr),
                mr_exact,
                mr_family,
                mr_deny,
            },
        );
    }
    let built = Arc::new(out);
    if let Ok(mut t) = tables.lock() {
        t.insert(grid.capture_id.clone(), built.clone());
    }
    built
}

/// The grid of the device this realm is WEARING, asked of the persona.
///
/// The I9 seam, the same one `install::surface::persona_surface` and every `Navigator` row read
/// through: the runtime PROVIDES the identity, the Web API READS it. There is no `KORVUS_BUNDLE`
/// here and there must never be — which bundle is being worn is the persona's business, and this asks
/// only what that bundle recorded.
///
/// `None` — a bare `JsRuntime` with no persona, or a persona with no bundle behind it — means this
/// process is not wearing a device, and the three members below then answer `""`/`false` for
/// everything. That is the honest answer for an engine with no recorded browser to speak for: it has
/// no demuxer and no decoder of its own (ruling R7), so with nothing to quote it claims nothing.
fn active_table(scope: &mut v8::PinScope) -> Option<Arc<Table>> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let grid = {
        let st = op_state.borrow();
        crate::binding::protos::persona_handle(&st)
            .map(|h| h.0.clone())?
            .codecs()?
    };
    Some(table_for(&grid))
}

// ---- Chromium's MimeUtil: the algorithm canPlayType and MediaSource.isTypeSupported run ----------

/// `media::internal::MimeUtil::Codec` (media/base/mime_util_internal.h).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum MCodec {
    Pcm,
    Mp3,
    Mpeg2Aac,
    Mpeg4Aac,
    Mpeg4XheAac,
    Vorbis,
    Opus,
    Flac,
    Vp8,
    Vp9,
    Theora,
    H264,
    Hevc,
    Av1,
    DolbyVision,
    Ac3,
    Eac3,
    Dts,
    DtsE,
    DtsXp2,
    Ac4,
    MpegH,
    Iamf,
}

impl MCodec {
    const ALL: [MCodec; 23] = [
        MCodec::Pcm,
        MCodec::Mp3,
        MCodec::Mpeg2Aac,
        MCodec::Mpeg4Aac,
        MCodec::Mpeg4XheAac,
        MCodec::Vorbis,
        MCodec::Opus,
        MCodec::Flac,
        MCodec::Vp8,
        MCodec::Vp9,
        MCodec::Theora,
        MCodec::H264,
        MCodec::Hevc,
        MCodec::Av1,
        MCodec::DolbyVision,
        MCodec::Ac3,
        MCodec::Eac3,
        MCodec::Dts,
        MCodec::DtsE,
        MCodec::DtsXp2,
        MCodec::Ac4,
        MCodec::MpegH,
        MCodec::Iamf,
    ];

    /// `MimeUtilToAudioCodec(codec) != AudioCodec::kUnknown`.
    fn is_audio(self) -> bool {
        use MCodec::*;
        matches!(self, Pcm | Mp3 | Mpeg2Aac | Mpeg4Aac | Mpeg4XheAac | Vorbis | Opus | Flac | Ac3 | Eac3 | Dts | DtsE | DtsXp2 | Ac4 | MpegH | Iamf)
    }

    /// What a desktop Linux Chrome 150 build decodes (`IsDefaultDecoderSupported{Audio,Video}Type` in
    /// media/base/supported_types.cc with Linux's build flags): the answer for a codec the worn device's
    /// capture never asked about. xHE-AAC needs the Windows/Mac/Android platform decoder, Theora has no decoder
    /// any more, HEVC is the GPU's (the device answers it), and AC-3/E-AC-3, DTS, AC-4, MPEG-H, IAMF and Dolby
    /// Vision are flags Linux builds leave off.
    fn linux_decodes(self) -> bool {
        use MCodec::*;
        matches!(self, Pcm | Mp3 | Mpeg2Aac | Mpeg4Aac | Vorbis | Opus | Flac | Vp8 | Vp9 | H264 | Av1)
    }
}

/// `GetStringToCodecMap()`: the codec strings that name a codec outright, with no parameters to parse.
fn simple_codec(s: &str) -> Option<MCodec> {
    use MCodec::*;
    Some(match s {
        "1" => Pcm,
        "mp3" | "mp4a.69" | "mp4a.6B" => Mp3,
        "mp4a.66" | "mp4a.67" | "mp4a.68" => Mpeg2Aac,
        "mp4a.40.2" | "mp4a.40.02" | "mp4a.40.5" | "mp4a.40.05" | "mp4a.40.29" => Mpeg4Aac,
        "mp4a.40.42" => Mpeg4XheAac,
        "ac-3" | "mp4a.a5" | "mp4a.A5" => Ac3,
        "ec-3" | "mp4a.a6" | "mp4a.A6" => Eac3,
        "vorbis" => Vorbis,
        "opus" | "Opus" => Opus,
        "flac" | "fLaC" => Flac,
        "vp8" | "vp8.0" => Vp8,
        "theora" => Theora,
        "dtsc" | "mp4a.a9" | "mp4a.A9" => Dts,
        "dtse" | "mp4a.ac" | "mp4a.AC" => DtsE,
        "dtsx" | "mp4a.b2" | "mp4a.B2" => DtsXp2,
        "ac-4" | "mp4a.ae" | "mp4a.AE" => Ac4,
        _ => return None,
    })
}

// `VideoCodecProfile` (media/base/video_codecs.h), the values the support check tells apart.
const PROFILE_UNKNOWN: i32 = -1;
const H264_BASELINE: i32 = 0;
const H264_MAIN: i32 = 1;
const H264_EXTENDED: i32 = 2;
const H264_HIGH: i32 = 3;
const H264_HIGH10: i32 = 4;
const H264_HIGH422: i32 = 5;
const H264_HIGH444: i32 = 6;
const H264_SCALABLE_BASELINE: i32 = 7;
const H264_SCALABLE_HIGH: i32 = 8;
const H264_STEREO_HIGH: i32 = 9;
const H264_MULTIVIEW_HIGH: i32 = 10;
const VP8_ANY: i32 = 11;
const VP9_PROFILE0: i32 = 12;
const VP9_PROFILE1: i32 = 13;
const VP9_PROFILE2: i32 = 14;
const VP9_PROFILE3: i32 = 15;
const HEVC_MAIN: i32 = 16;
const HEVC_MAIN10: i32 = 17;
const HEVC_MAIN_STILL_PICTURE: i32 = 18;
const THEORA_ANY: i32 = 23;
const AV1_MAIN: i32 = 24;
const AV1_HIGH: i32 = 25;
const AV1_PRO: i32 = 26;
const HEVC_REXT: i32 = 29;
const HEVC_HIGH_THROUGHPUT: i32 = 30;
const HEVC_MULTIVIEW_MAIN: i32 = 31;
const HEVC_SCALABLE_MAIN: i32 = 32;
const HEVC_3D_MAIN: i32 = 33;
const HEVC_SCREEN_EXTENDED: i32 = 34;
const HEVC_SCALABLE_REXT: i32 = 35;
const HEVC_HIGH_THROUGHPUT_SCREEN_EXTENDED: i32 = 36;
/// Dolby Vision's profiles all reach the same answer (no Linux build decodes it); one value stands for them.
const DOLBY_VISION_ANY: i32 = 19;

/// `media::VideoType`, reduced to what the support check reads.
#[derive(Clone, Copy, Debug)]
struct VideoType {
    profile: i32,
    level: u32,
}

/// `MimeUtil::ParsedCodecResult`.
#[derive(Clone, Copy, Debug)]
struct ParsedCodec {
    codec: MCodec,
    ambiguous: bool,
    video: Option<VideoType>,
}

/// base/strings/string_number_conversions_internal.h's `StringToNumber`: leading ASCII whitespace parses
/// and makes the result invalid, a leading `+` is allowed, a `-` only for a signed target, base 16 skips a
/// `0x`/`0X` prefix, every remaining byte must be a digit of the base, and the value must fit `int` / `unsigned`.
/// `StringToInt` is `(10, true)`, `StringToUint` `(10, false)`, `HexStringToUInt` `(16, false)`.
fn chromium_number(s: &str, radix: u32, signed: bool) -> Option<i64> {
    let b = s.as_bytes();
    let mut i = 0;
    let mut leading_ws = false;
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        leading_ws = true;
        i += 1;
    }
    let mut negative = false;
    if i < b.len() && b[i] == b'-' {
        if !signed {
            return None;
        }
        negative = true;
        i += 1;
    } else if i < b.len() && b[i] == b'+' {
        i += 1;
    }
    let mut body = &b[i..];
    if radix == 16 && body.len() > 2 && body[0] == b'0' && (body[1] == b'x' || body[1] == b'X') {
        body = &body[2..];
    }
    if body.is_empty() {
        return None;
    }
    let (lo, hi) = if signed { (i32::MIN as i64, i32::MAX as i64) } else { (0, u32::MAX as i64) };
    let mut v: i64 = 0;
    for &c in body {
        let d = (c as char).to_digit(radix)? as i64;
        v = v * radix as i64 + if negative { -d } else { d };
        if v < lo || v > hi {
            return None;
        }
    }
    if leading_ws {
        return None;
    }
    Some(v)
}

/// `IsValidH264Level` (Table A-1 of ISO/IEC 14496-10).
fn valid_h264_level(l: u32) -> bool {
    matches!(l, 10..=13 | 20..=22 | 30..=32 | 40..=42 | 50..=52 | 60..=62)
}

/// `VideoColorSpace::GetPrimaryID` / `GetTransferID` / `GetMatrixID` do not answer INVALID.
fn valid_primaries(n: i64) -> bool {
    ((1..=12).contains(&n) && n != 3) || n == 22
}
fn valid_transfer(n: i64) -> bool {
    (1..=18).contains(&n) && n != 3
}
fn valid_matrix(n: i64) -> bool {
    (0..=11).contains(&n) && n != 3 && n != 10
}

/// `ParseNewStyleVp9CodecID`: `vp09.PP.LL.DD[.CC[.cp[.tc[.mc[.FF]]]]]`.
fn parse_vp9_new(c: &str) -> Option<VideoType> {
    let f: Vec<&str> = c.split('.').collect();
    if f.len() < 4 || f.len() > 9 || f[0] != "vp09" {
        return None;
    }
    let mut v = Vec::with_capacity(f.len() - 1);
    for x in &f[1..] {
        if x.is_empty() {
            return None;
        }
        let n = chromium_number(x, 10, true)?;
        if n < 0 {
            return None;
        }
        v.push(n);
    }
    let profile = match v[0] {
        0 => VP9_PROFILE0,
        1 => VP9_PROFILE1,
        2 => VP9_PROFILE2,
        3 => VP9_PROFILE3,
        _ => return None,
    };
    if !matches!(v[1], 10 | 11 | 20 | 21 | 30 | 31 | 40 | 41 | 50 | 51 | 52 | 60 | 61 | 62) {
        return None;
    }
    if !matches!(v[2], 8 | 10 | 12) {
        return None;
    }
    // chroma subsampling, then the colour description, then the full-range flag: each checked when present
    if v.len() > 3 && !(0..=3).contains(&v[3]) {
        return None;
    }
    if v.len() > 4 && !valid_primaries(v[4]) {
        return None;
    }
    if v.len() > 5 && !valid_transfer(v[5]) {
        return None;
    }
    if v.len() > 6 && !valid_matrix(v[6]) {
        return None;
    }
    if v.len() > 7 && v[7] > 1 {
        return None;
    }
    Some(VideoType { profile, level: v[1] as u32 })
}

/// `ParseAv1CodecId`: `av01.P.LLT.DD[.M.CCC.cp.tc.mc.F]`.
fn parse_av1(c: &str) -> Option<VideoType> {
    let mut f: Vec<&str> = c.split('.').collect();
    if f.len() < 4 || f.len() > 10 || f[0] != "av01" {
        return None;
    }
    let level_tier: &str = f[2];
    let lt = level_tier.as_bytes();
    if lt.len() != 3 || (lt[2] != b'M' && lt[2] != b'H') {
        return None;
    }
    f[2] = &level_tier[..2];
    let mut v = vec![0i64; f.len()];
    for (i, field) in f.iter().enumerate().skip(1) {
        if field.is_empty() {
            return None;
        }
        v[i] = chromium_number(field, 10, true)?;
        if v[i] < 0 {
            return None;
        }
    }
    let profile = match if f[1].len() == 1 { v[1] } else { -1 } {
        0 => AV1_MAIN,
        1 => AV1_HIGH,
        2 => AV1_PRO,
        _ => return None,
    };
    if v[2] > 31 {
        return None;
    }
    if f[3].len() != 2 || !matches!(v[3], 8 | 10 | 12) {
        return None;
    }
    let out = Some(VideoType { profile, level: v[2] as u32 });
    if f.len() <= 4 {
        return out;
    }
    let mono = v[4];
    if f[4].len() != 1 || mono > 1 || (mono == 1 && profile == AV1_HIGH) {
        return None;
    }
    if f.len() <= 5 {
        return out;
    }
    let cs = f[5].as_bytes();
    if cs.len() != 3 {
        return None;
    }
    let (sx, sy, pos) = (cs[0], cs[1], cs[2]);
    if !(b'0'..=b'1').contains(&sx) || !(b'0'..=b'1').contains(&sy) || !(b'0'..=b'3').contains(&pos) {
        return None;
    }
    if (sx == b'0' || sy == b'0') && pos != b'0' {
        return None;
    }
    if f.len() <= 6 {
        return out;
    }
    if f[6].len() != 2 || !valid_primaries(v[6]) {
        return None;
    }
    if f.len() <= 7 {
        return out;
    }
    if f[7].len() != 2 || !valid_transfer(v[7]) {
        return None;
    }
    if f.len() <= 8 {
        return out;
    }
    if f[8].len() != 2 || !valid_matrix(v[8]) {
        return None;
    }
    if f.len() <= 9 {
        return out;
    }
    if f[9].len() != 1 || v[9] > 1 {
        return None;
    }
    out
}

/// `ParseAVCCodecId`: `avc1.PPCCLL` / `avc3.PPCCLL`, the constraint flags narrowing the profile.
fn parse_avc(c: &str) -> Option<VideoType> {
    if !(c.starts_with("avc1.") || c.starts_with("avc3.")) || c.len() != 11 {
        return None;
    }
    let e = chromium_number(c.get(5..)?, 16, false)? as u32;
    let level = e & 0xff;
    let constraints = (e >> 8) & 0xff;
    let profile_idc = (e >> 16) & 0xff;
    if constraints & 3 != 0 {
        return None;
    }
    let mut profile = match profile_idc {
        66 => H264_BASELINE,
        77 => H264_MAIN,
        83 => H264_SCALABLE_BASELINE,
        86 => H264_SCALABLE_HIGH,
        88 => H264_EXTENDED,
        100 => H264_HIGH,
        110 => H264_HIGH10,
        118 => H264_MULTIVIEW_HIGH,
        122 => H264_HIGH422,
        128 => H264_STEREO_HIGH,
        244 => H264_HIGH444,
        _ => return None,
    };
    if (constraints >> 5) & 1 == 1 && profile > H264_EXTENDED {
        profile = H264_EXTENDED;
    }
    if (constraints >> 6) & 1 == 1 && profile > H264_MAIN {
        profile = H264_MAIN;
    }
    if (constraints >> 7) & 1 == 1 && profile > H264_BASELINE {
        profile = H264_BASELINE;
    }
    Some(VideoType { profile, level })
}

/// `ParseHEVCCodecId` (ISO/IEC 14496-15 Annex E.3): `hev1|hvc1.[ABC]P.FLAGS.[LH]LEVEL[.CC]*`.
fn parse_hevc(c: &str) -> Option<VideoType> {
    if !(c.starts_with("hev1.") || c.starts_with("hvc1.")) || c.len() > 5 + 4 + 9 + 5 + 18 {
        return None;
    }
    let ws: &[char] = &[' ', '\t', '\n', '\x0b', '\x0c', '\r'];
    let e: Vec<&str> = c.split('.').map(|x| x.trim_matches(ws)).collect();
    if e.len() < 4 {
        return None;
    }
    let p = e[1].strip_prefix(['A', 'B', 'C']).unwrap_or(e[1]);
    let idc = chromium_number(p, 10, false)?;
    if idc > 0x1f {
        return None;
    }
    let flags = chromium_number(e[2], 16, false)?;
    // Spec A.3 and Annexes G-I, in Chromium's order: a later match overrides an earlier one.
    let mut profile = PROFILE_UNKNOWN;
    for (n, bit, prof) in [
        (11, 2048, HEVC_HIGH_THROUGHPUT_SCREEN_EXTENDED),
        (10, 1024, HEVC_SCALABLE_REXT),
        (9, 512, HEVC_SCREEN_EXTENDED),
        (8, 256, HEVC_3D_MAIN),
        (7, 128, HEVC_SCALABLE_MAIN),
        (6, 64, HEVC_MULTIVIEW_MAIN),
        (5, 32, HEVC_HIGH_THROUGHPUT),
        (4, 16, HEVC_REXT),
        (2, 4, HEVC_MAIN10),
        (1, 2, HEVC_MAIN),
        (3, 8, HEVC_MAIN_STILL_PICTURE),
    ] {
        if idc == n || flags & bit != 0 {
            profile = prof;
        }
    }
    if profile == PROFILE_UNKNOWN {
        return None;
    }
    let tier_level = e[3];
    if !tier_level.starts_with(['L', 'H']) {
        return None;
    }
    let level = chromium_number(&tier_level[1..], 10, false)?;
    if level > 0xff || e.len() > 10 {
        return None;
    }
    let mut constraint = [0i64; 6];
    for (slot, field) in constraint.iter_mut().zip(&e[4..]) {
        let b = chromium_number(field, 16, false)?;
        if b > 0xff {
            return None;
        }
        *slot = b;
    }
    // kHEVCRextCodecStringParsing (enabled by default): a range-extension string's bit-depth and chroma flags
    // must name a format.
    if profile == HEVC_REXT {
        let max_bits = (constraint[0] >> 1) & 0b111;
        let max_422 = constraint[0] & 1 == 1;
        let max_420 = (constraint[1] >> 7) & 1 == 1;
        let max_mono = (constraint[1] >> 6) & 1 == 1;
        if !matches!(max_bits, 0b000 | 0b100 | 0b110 | 0b111) {
            return None;
        }
        if (max_mono && (!max_422 || !max_420)) || (max_420 && !max_422) {
            return None;
        }
    }
    Some(VideoType { profile, level: level as u32 })
}

/// `ParseDolbyVisionCodecId`: `dva1|dvav|dvh1|dvhe|dav1.PP.LL`, the profile matching the base codec.
fn parse_dolby_vision(c: &str) -> Option<VideoType> {
    let avc = c.starts_with("dva1.") || c.starts_with("dvav.");
    let hevc = c.starts_with("dvh1.") || c.starts_with("dvhe.");
    let av1 = c.starts_with("dav1.");
    if !(avc || hevc || av1) || c.len() > 10 {
        return None;
    }
    let ws: &[char] = &[' ', '\t', '\n', '\x0b', '\x0c', '\r'];
    let e: Vec<&str> = c.split('.').map(|x| x.trim_matches(ws)).collect();
    if e.len() != 3 || e[1].len() != 2 {
        return None;
    }
    let profile = chromium_number(e[1], 10, false)?;
    let base_ok = match profile {
        0 | 9 => avc,
        5 | 7 | 8 | 20 => hevc,
        10 => av1,
        _ => false,
    };
    if !base_ok || e[2].len() != 2 {
        return None;
    }
    let level = chromium_number(e[2], 10, false)?;
    if !(1..=13).contains(&level) {
        return None;
    }
    Some(VideoType { profile: DOLBY_VISION_ANY, level: level as u32 })
}

/// `MimeUtil::ParseCodecHelper`: the map's plain names, the three strings allowed to be ambiguous, then the
/// parameterised families. `mime` is lower-cased; only `video/webm` accepts the legacy `vp9` / `vp9.0`.
fn parse_codec_helper(mime: &str, c: &str) -> Option<ParsedCodec> {
    if let Some(codec) = simple_codec(c) {
        let video = match codec {
            MCodec::Vp8 => Some(VideoType { profile: VP8_ANY, level: 0 }),
            MCodec::Theora => Some(VideoType { profile: THEORA_ANY, level: 0 }),
            _ => None,
        };
        return Some(ParsedCodec { codec, ambiguous: false, video });
    }
    if c == "avc1" || c == "avc3" {
        return Some(ParsedCodec { codec: MCodec::H264, ambiguous: true, video: Some(VideoType { profile: PROFILE_UNKNOWN, level: 0 }) });
    }
    if c == "mp4a.40" {
        return Some(ParsedCodec { codec: MCodec::Mpeg4Aac, ambiguous: true, video: None });
    }
    if let Some(v) = parse_vp9_new(c) {
        return Some(ParsedCodec { codec: MCodec::Vp9, ambiguous: false, video: Some(v) });
    }
    if mime == "video/webm" && (c == "vp9" || c == "vp9.0") {
        return Some(ParsedCodec { codec: MCodec::Vp9, ambiguous: true, video: Some(VideoType { profile: PROFILE_UNKNOWN, level: 0 }) });
    }
    if let Some(v) = parse_av1(c) {
        return Some(ParsedCodec { codec: MCodec::Av1, ambiguous: false, video: Some(v) });
    }
    if let Some(v) = parse_avc(c) {
        // "Allowed string ambiguity since 2014": a level outside Table A-1 makes the string ambiguous.
        return Some(ParsedCodec { codec: MCodec::H264, ambiguous: !valid_h264_level(v.level), video: Some(v) });
    }
    if let Some(v) = parse_hevc(c) {
        return Some(ParsedCodec { codec: MCodec::Hevc, ambiguous: false, video: Some(v) });
    }
    if let Some(v) = parse_dolby_vision(c) {
        return Some(ParsedCodec { codec: MCodec::DolbyVision, ambiguous: false, video: Some(v) });
    }
    // `mhm1.`/`mha1.`, AC-4's and IAMF's long forms are parsed only by builds with those platform flags, which
    // Linux Chrome has off.
    None
}

/// What the WORN DEVICE contributes to Chromium's algorithm -- everything that is a fact of a build or a
/// machine rather than of the algorithm -- derived from its recorded grid by [`derive_device`].
#[derive(Debug)]
pub(crate) struct Device {
    /// `media_format_map_` as this device's build filled it (`AddSupportedMediaFormats` under its build flags),
    /// plus whatever its vendor added.
    containers: HashMap<String, HashSet<MCodec>>,
    /// `IsDecoderSupported{Audio,Video}Type`, per codec.
    decodes: HashSet<MCodec>,
    /// `IsDecoderBuiltInVideoCodec(kH264)`: H.264 High 10 is then a plain "probably".
    h264_built_in: bool,
    /// libvpx's `VPX_CODEC_CAP_HIGHBITDEPTH`: VP9 profiles 2 and 3.
    vp9_high_bit_depth: bool,
    /// The HEVC profiles the machine's hardware decodes (`GetSupplementalDecoderVideoProfileCache`).
    hevc_profiles: HashSet<i32>,
    /// `USE_PROPRIETARY_CODECS`: the MP4 family's H.264/AAC entries, `audio/aac`, MSE's MPEG-2 TS parser.
    proprietary: bool,
    /// `ENABLE_PLATFORM_HEVC` / `ENABLE_AV1_DECODER`: MSE's `hev1.*`/`hvc1.*` and `av01.*` patterns.
    platform_hevc: bool,
    av1: bool,
}

/// The media build flags `AddSupportedMediaFormats()` reads (media/media_options.gni).
#[derive(Clone, Copy, Debug)]
struct BuildFlags {
    /// `USE_PROPRIETARY_CODECS`
    proprietary: bool,
    /// `ENABLE_HLS_DEMUXER`
    hls: bool,
    /// `ENABLE_PLATFORM_HEVC`
    hevc: bool,
    /// `ENABLE_AV1_DECODER`
    av1: bool,
    /// `ENABLE_PLATFORM_AC3_EAC3_AUDIO`
    ac3: bool,
    /// `ENABLE_PLATFORM_AC4_AUDIO`
    ac4: bool,
    /// `ENABLE_PLATFORM_MPEG_H_AUDIO`
    mpegh: bool,
    /// `ENABLE_PLATFORM_DOLBY_VISION`
    dolby_vision: bool,
    /// `ENABLE_PLATFORM_DTS_AUDIO`
    dts: bool,
    /// `ENABLE_PLATFORM_IAMF_AUDIO`
    iamf: bool,
}

/// `AddSupportedMediaFormats()` under a build's flags.
fn chromium_container_map(flags: BuildFlags) -> HashMap<String, HashSet<MCodec>> {
    use MCodec::*;
    let BuildFlags { proprietary, hls, hevc, av1, ac3, ac4, mpegh, dolby_vision, dts, iamf } = flags;
    let set = |c: &[MCodec]| -> HashSet<MCodec> { c.iter().copied().collect() };
    let wav = set(&[Pcm]);
    let ogg_audio = set(&[Flac, Opus, Vorbis]);
    let ogg = set(&[Flac, Opus, Vorbis, Vp8]);
    let webm_audio = set(&[Opus, Vorbis]);
    let mut webm = set(&[Opus, Vorbis, Vp8, Vp9]);
    let mut mp4_audio = set(&[Flac, Mp3, Opus]);
    let mut mkv_audio = set(&[Pcm, Flac, Mp3, Opus, Vorbis]);
    let mut mp4_video = set(&[Vp9]);
    let mut mkv_video = set(&[Vp8, Vp9]);
    if av1 {
        webm.insert(Av1);
    }
    let aac = set(&[Mpeg2Aac, Mpeg4Aac, Mpeg4XheAac]);
    if proprietary {
        mp4_audio.extend(&aac);
        mkv_audio.extend(&aac);
        if ac3 {
            mp4_audio.extend([Ac3, Eac3]);
            mkv_audio.extend([Ac3, Eac3]);
        }
        if ac4 {
            mp4_audio.insert(Ac4);
            mkv_audio.insert(Ac4);
        }
        if mpegh {
            mp4_audio.insert(MpegH);
            mkv_audio.insert(MpegH);
        }
        mp4_video.insert(H264);
        mkv_video.insert(H264);
        if hevc {
            mp4_video.insert(Hevc);
            mkv_video.insert(Hevc);
        }
        if dolby_vision {
            mp4_video.insert(DolbyVision);
            mkv_video.insert(DolbyVision);
        }
    }
    if av1 {
        mp4_video.insert(Av1);
        mkv_video.insert(Av1);
    }
    if dts {
        mp4_audio.extend([Dts, DtsXp2, DtsE]);
        mkv_audio.extend([Dts, DtsXp2, DtsE]);
    }
    if iamf {
        mp4_audio.insert(Iamf);
    }
    let mp4: HashSet<MCodec> = mp4_audio.union(&mp4_video).copied().collect();
    let mkv: HashSet<MCodec> = mkv_audio.union(&mkv_video).copied().collect();
    let mut m: HashMap<String, HashSet<MCodec>> = HashMap::new();
    let mut add = |t: &str, s: &HashSet<MCodec>| {
        m.insert(t.to_string(), s.clone());
    };
    add("audio/wav", &wav);
    add("audio/x-wav", &wav);
    add("audio/webm", &webm_audio);
    add("video/webm", &webm);
    add("audio/ogg", &ogg_audio);
    add("video/ogg", &ogg);
    add("application/ogg", &ogg);
    // The implicit-codec containers carry an EMPTY set: `GetDefaultCodec` names their one codec.
    let implicit = HashSet::new();
    add("audio/flac", &implicit);
    add("audio/mpeg", &set(&[Mp3]));
    add("audio/mp3", &implicit);
    add("audio/x-mp3", &implicit);
    add("audio/mp4", &mp4_audio);
    add("video/mp4", &mp4);
    add("audio/matroska", &mkv_audio);
    add("video/matroska", &mkv);
    add("audio/x-matroska", &mkv_audio);
    add("video/x-matroska", &mkv);
    if proprietary {
        add("audio/aac", &implicit);
        add("audio/x-m4a", &aac);
        let mut avc_and_aac = aac.clone();
        avc_and_aac.insert(H264);
        add("video/x-m4v", &avc_and_aac);
        add("video/3gpp", &avc_and_aac);
        if hls {
            let hls_codecs = set(&[H264, Mp3, Mpeg4Aac]);
            for t in ["application/x-mpegurl", "application/vnd.apple.mpegurl", "audio/mpegurl", "audio/x-mpegurl"] {
                add(t, &hls_codecs);
            }
        }
    }
    m
}

/// The device facts a recorded grid holds, read the way Chromium's build would have produced them:
///
///   * a codec is decoded when some spelling of it answered "probably" in some container, and not when the
///     sweep asked about it and nothing accepted it (Opera 135's Linux build: no H.264, no AAC); a codec the
///     sweep never asked about is answered as desktop Linux Chrome 150 builds it ([`MCodec::linux_decodes`]);
///   * the build flags follow: `USE_PROPRIETARY_CODECS` from the containers only it registers, the HLS demuxer
///     from the `mpegurl` containers, `ENABLE_PLATFORM_HEVC` and friends from their codecs;
///   * the VP9 high-bit-depth and HEVC profile answers are the accepted spellings' profiles;
///   * a vendor's own container table (Edge 151's `audio/3gpp`) is whatever recorded container Chromium's map
///     lacks, or a codec a recorded container accepts that Chromium's map does not list for it.
///
/// `None` for a grid that records no container at all -- a device that claims nothing (ruling R7).
fn derive_device(grid: &spec::CodecGrid) -> Option<Device> {
    let present: Vec<(&String, &spec::CodecContainer)> =
        grid.containers.iter().filter(|(_, c)| !c.base_video.is_empty() || !c.video.is_empty()).collect();
    if present.is_empty() {
        return None;
    }
    let mut accepted: HashMap<MCodec, HashSet<i32>> = HashMap::new();
    let mut per_container: HashMap<String, HashSet<MCodec>> = HashMap::new();
    for (container, row) in &present {
        let mut set = HashSet::new();
        for spelling in &row.video {
            let Some(p) = parse_codec_helper(container, spelling) else { continue };
            if p.ambiguous {
                continue;
            }
            accepted.entry(p.codec).or_default().insert(p.video.map(|v| v.profile).unwrap_or(PROFILE_UNKNOWN));
            set.insert(p.codec);
        }
        per_container.insert(container.to_string(), set);
    }
    // `video/webm` is the one container that parses every spelling, the legacy VP9 one included.
    let asked: HashSet<MCodec> =
        grid.asked.iter().filter_map(|s| parse_codec_helper("video/webm", s)).filter(|p| !p.ambiguous).map(|p| p.codec).collect();
    let decodes: HashSet<MCodec> = MCodec::ALL
        .iter()
        .copied()
        .filter(|c| if asked.contains(c) || grid.asked.is_empty() { accepted.contains_key(c) } else { c.linux_decodes() })
        .collect();
    let has = |names: &[&str]| present.iter().any(|(c, _)| names.contains(&c.as_str()));
    let d = |c: MCodec| decodes.contains(&c);
    let proprietary = has(&["audio/aac", "audio/x-m4a", "video/x-m4v", "video/3gpp"]);
    let hls = has(&["application/x-mpegurl", "application/vnd.apple.mpegurl", "audio/mpegurl", "audio/x-mpegurl"]);
    let (platform_hevc, av1) = (d(MCodec::Hevc), d(MCodec::Av1));
    let mut containers = chromium_container_map(BuildFlags {
        proprietary,
        hls,
        hevc: platform_hevc,
        av1,
        ac3: d(MCodec::Ac3) || d(MCodec::Eac3),
        ac4: d(MCodec::Ac4),
        mpegh: d(MCodec::MpegH),
        dolby_vision: d(MCodec::DolbyVision),
        dts: d(MCodec::Dts) || d(MCodec::DtsE) || d(MCodec::DtsXp2),
        iamf: d(MCodec::Iamf),
    });
    for (container, set) in per_container {
        match containers.get_mut(&container) {
            Some(known) => known.extend(set),
            None => {
                containers.insert(container, set);
            }
        }
    }
    let vp9 = accepted.get(&MCodec::Vp9);
    Some(Device {
        containers,
        h264_built_in: d(MCodec::H264),
        vp9_high_bit_depth: if asked.contains(&MCodec::Vp9) {
            vp9.is_some_and(|p| p.contains(&VP9_PROFILE2) || p.contains(&VP9_PROFILE3))
        } else {
            true
        },
        hevc_profiles: accepted.get(&MCodec::Hevc).cloned().unwrap_or_default(),
        decodes,
        proprietary,
        platform_hevc,
        av1,
    })
}

/// THE ACTIVE DEVICE, memoised by capture id the way [`table_for`] memoises the reduced table.
fn device_for(grid: &spec::CodecGrid) -> Option<Arc<Device>> {
    static DEVICES: OnceLock<Mutex<HashMap<String, Option<Arc<Device>>>>> = OnceLock::new();
    let devices = DEVICES.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(d) = devices.lock() {
        if let Some(hit) = d.get(&grid.capture_id) {
            return hit.clone();
        }
    }
    let built = derive_device(grid).map(Arc::new);
    if let Ok(mut d) = devices.lock() {
        d.insert(grid.capture_id.clone(), built.clone());
    }
    built
}

fn active_device(scope: &mut v8::PinScope) -> Option<Arc<Device>> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let grid = {
        let st = op_state.borrow();
        crate::binding::protos::persona_handle(&st).map(|h| h.0.clone())?.codecs()?
    };
    device_for(&grid)
}

/// `media::SupportsType`, as `canPlayType` spells it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Supports {
    Not,
    Maybe,
    Probably,
}

impl Device {
    /// `IsDecoderSupported{Audio,Video}Type` for one parsed codec.
    fn decoder_supported(&self, codec: MCodec, video: Option<VideoType>) -> bool {
        match codec {
            MCodec::Vp9 => {
                let p = video.map(|v| v.profile).unwrap_or(VP9_PROFILE0);
                self.decodes.contains(&MCodec::Vp9) && (!matches!(p, VP9_PROFILE2 | VP9_PROFILE3) || self.vp9_high_bit_depth)
            }
            MCodec::Hevc => video.is_some_and(|v| self.hevc_profiles.contains(&v.profile)),
            other => self.decodes.contains(&other),
        }
    }

    /// `MimeUtil::IsCodecSupported`: H.264 profiles outside Baseline/Main/High (High 10 too, without a
    /// built-in decoder) are "maybe" -- "only supported on some hardware".
    fn codec_supported(&self, codec: MCodec, video: Option<VideoType>) -> Supports {
        let mut ambiguous_platform = false;
        if codec == MCodec::H264 {
            match video.map(|v| v.profile).unwrap_or(PROFILE_UNKNOWN) {
                H264_BASELINE | H264_MAIN | H264_HIGH => {}
                H264_HIGH10 if self.h264_built_in => {}
                _ => ambiguous_platform = true,
            }
        }
        if !self.decoder_supported(codec, video) {
            return Supports::Not;
        }
        if ambiguous_platform { Supports::Maybe } else { Supports::Probably }
    }

    /// `MimeUtil::IsSupportedMediaFormat` (with `ParseCodecStrings` and `AreSupportedCodecs`), not encrypted.
    fn supported_media_format(&self, mime: &str, codecs: &[String]) -> Supports {
        if mime.is_empty() && codecs.is_empty() {
            return Supports::Not;
        }
        let mut valid: Option<&HashSet<MCodec>> = None;
        if !mime.is_empty() {
            let Some(set) = self.containers.get(mime) else { return Supports::Not };
            if set.is_empty() {
                // A container that implies its codec takes no `codecs` parameter.
                if !codecs.is_empty() {
                    return Supports::Not;
                }
                return match default_codec(mime) {
                    Some(c) => self.codec_supported(c, None),
                    None => Supports::Not,
                };
            }
            if codecs.is_empty() {
                // Codecs expected and none given: the implied codec, or "maybe" when there is none.
                return match default_codec(mime) {
                    Some(c) => self.codec_supported(c, None),
                    None => Supports::Maybe,
                };
            }
            valid = Some(set);
        }
        let mut parsed = Vec::with_capacity(codecs.len());
        for c in codecs {
            let Some(p) = parse_codec_helper(mime, c) else { return Supports::Not };
            if valid.is_some_and(|v| !v.contains(&p.codec)) {
                return Supports::Not;
            }
            parsed.push(p);
        }
        let mut combined = Supports::Probably;
        for p in parsed {
            // "Make conservative guesses to resolve ambiguity before checking platform support."
            let mut video = p.video;
            if p.ambiguous {
                if let Some(v) = video.as_mut() {
                    match p.codec {
                        MCodec::H264 => {
                            if v.profile == PROFILE_UNKNOWN {
                                v.profile = H264_BASELINE;
                            }
                            if !valid_h264_level(v.level) {
                                v.level = 10;
                            }
                        }
                        MCodec::Vp9 => {
                            if v.profile == PROFILE_UNKNOWN {
                                v.profile = VP9_PROFILE0;
                            }
                            if v.level == 0 {
                                v.level = 10;
                            }
                        }
                        _ => {}
                    }
                }
            }
            match self.codec_supported(p.codec, video) {
                Supports::Not => return Supports::Not,
                // An ambiguous string downgrades to "maybe" -- except the legacy `vp9`, which "historically
                // returned Probably" and keeps it.
                Supports::Maybe => combined = Supports::Maybe,
                Supports::Probably if p.ambiguous && p.codec != MCodec::Vp9 => combined = Supports::Maybe,
                Supports::Probably => {}
            }
        }
        combined
    }
}

/// `MimeUtil::GetDefaultCodec`: the codec a container's name implies unambiguously.
fn default_codec(mime: &str) -> Option<MCodec> {
    match mime {
        "audio/mpeg" | "audio/mp3" | "audio/x-mp3" => Some(MCodec::Mp3),
        "audio/aac" => Some(MCodec::Mpeg4Aac),
        "audio/flac" => Some(MCodec::Flac),
        _ => None,
    }
}

const ASCII_WS: &[char] = &[' ', '\t', '\n', '\x0b', '\x0c', '\r'];

/// WTF's `IsSpaceOrNewline`, the predicate `StripWhiteSpace()` strips: ASCII whitespace, and a non-ASCII
/// character whose bidi class is WS.
fn blink_space(c: char) -> bool {
    if c.is_ascii() {
        ASCII_WS.contains(&c)
    } else {
        matches!(c, '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{205f}' | '\u{3000}')
    }
}

/// `String::DeprecatedLower()`: each character's simple lower-case mapping.
fn deprecated_lower(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c == '\u{130}' {
                return 'i';
            }
            let mut l = c.to_lowercase();
            match (l.next(), l.next()) {
                (Some(x), None) => x,
                _ => c,
            }
        })
        .collect()
}

/// Blink's `ContentType` (platform/network/mime/content_type.cc): the whitespace-stripped string, its type the
/// part before the first `;`, and the first parameter named `codecs` (case-insensitively) with every `"`
/// removed from its value. Parameters split on `;` outside double quotes. `None` codecs = no such parameter.
fn blink_content_type(input: &str) -> (String, Option<String>) {
    let t = input.trim_matches(blink_space);
    let ty = match t.find(';') {
        Some(i) => t[..i].trim_matches(blink_space).to_string(),
        None => t.to_string(),
    };
    let mut params = Vec::new();
    let (mut start, mut quoted) = (0usize, false);
    for (i, ch) in t.char_indices() {
        if !quoted && ch == ';' {
            if i != start {
                params.push(&t[start..i]);
            }
            start = i + 1;
        } else if ch == '"' {
            quoted = !quoted;
        }
    }
    if start != t.len() {
        params.push(&t[start..]);
    }
    for p in params {
        let p = p.trim_matches(blink_space);
        if let Some(eq) = p.find('=') {
            if p[..eq].trim_matches(blink_space).eq_ignore_ascii_case("codecs") {
                return (ty, Some(p[eq + 1..].trim_matches(blink_space).replace('"', "")));
            }
        }
    }
    (ty, None)
}

/// `media::SplitCodecs`: quotes trimmed, split on `,`, each trimmed; an empty or all-whitespace list is no codecs.
fn split_codecs(codecs: &str) -> Vec<String> {
    let out: Vec<String> = codecs.trim_matches('"').split(',').map(|c| c.trim_matches(ASCII_WS).to_string()).collect();
    if out.len() == 1 && out[0].is_empty() { Vec::new() } else { out }
}

/// `HTMLMediaElement::canPlayType` -> `GetSupportsType` -> `MIMETypeRegistry::SupportsMediaMIMEType`.
fn chromium_can_play(device: &Device, input: &str) -> Supports {
    let (ty, codecs) = blink_content_type(input);
    if ty.is_empty() {
        return Supports::Not;
    }
    // `DeprecatedLower()`, then `ToLowerASCIIOrEmpty`: a type with a non-ASCII character is the empty type.
    let lower = deprecated_lower(&ty);
    if lower == "application/octet-stream" {
        return Supports::Not;
    }
    let mime = if lower.is_ascii() { lower } else { String::new() };
    // The codecs keep their case (RFC 4281 makes them case-sensitive); non-ASCII ones are no codecs at all.
    let list = match codecs {
        Some(c) if c.is_ascii() => split_codecs(&c),
        _ => Vec::new(),
    };
    device.supported_media_format(&mime, &list)
}

/// `StreamParserFactory`'s `kSupportedTypeInfo`: the MSE byte-stream formats and the codec patterns each takes
/// (a trailing `*` matches any rest). `None` = an implicit-codec format, which takes no `codecs` parameter.
fn mse_patterns(device: &Device, mime: &str) -> Option<Option<Vec<&'static str>>> {
    let p = device.proprietary;
    Some(match mime {
        "video/webm" => {
            let mut v = vec!["vp8", "vp9", "vp09.*", "vorbis", "opus", "Opus"];
            if device.av1 {
                v.push("av01.*");
            }
            Some(v)
        }
        "audio/webm" => Some(vec!["vorbis", "opus", "Opus"]),
        "audio/mpeg" => None,
        "video/mp4" => {
            let mut v = vec!["flac", "fLaC", "opus", "Opus", "vp09.*"];
            if p {
                v.extend(["avc1.*", "avc3.*"]);
                if device.platform_hevc {
                    v.extend(["hev1.*", "hvc1.*"]);
                }
                v.extend(["mp4a.40.*", "mp4a.67"]);
            }
            if device.av1 {
                v.push("av01.*");
            }
            Some(v)
        }
        "audio/mp4" => {
            let mut v = vec!["flac", "fLaC", "opus", "Opus"];
            if p {
                v.extend(["mp4a.40.*", "mp4a.67"]);
            }
            Some(v)
        }
        "audio/aac" if p => None,
        _ => return None,
    })
}

/// `GetMP4AudioObjectType`: the third field of `mp4a.40.N`, or -1.
fn mp4_audio_object_type(c: &str) -> i64 {
    let t: Vec<&str> = c.split('.').filter(|s| !s.is_empty()).collect();
    if t.len() == 3 && t[0] == "mp4a" && t[1] == "40" {
        return chromium_number(t[2], 10, true).unwrap_or(-1);
    }
    -1
}

/// `StreamParserFactory::IsTypeSupported(...) == kSupported` (`CheckTypeAndCodecs`).
fn stream_parser_supported(device: &Device, mime: &str, codecs: &[String]) -> bool {
    let Some(patterns) = mse_patterns(device, mime) else { return false };
    let Some(patterns) = patterns else {
        // An implicit codec: supported with no `codecs` parameter, and with any it is not.
        return codecs.is_empty();
    };
    if codecs.is_empty() {
        // "A codecs parameter must be provided ... to determine definitive support": kMaybeSupported.
        return false;
    }
    codecs.iter().all(|c| {
        // `base::MatchPattern`, first match only ("only 1 pattern will match").
        let hit = patterns.iter().copied().find(|p| match p.strip_suffix('*') {
            Some(prefix) => c.starts_with(prefix),
            None => c.as_str() == *p,
        });
        match hit {
            // `ValidateMP4ACodecID`: AAC-LC, HE-AAC, HE-AACv2 and xHE-AAC.
            Some("mp4a.40.*") => matches!(mp4_audio_object_type(c), 2 | 5 | 29 | 42),
            Some(_) => true,
            None => false,
        }
    })
}

/// `IsMp2tCodecSupported` (third_party/blink/renderer/modules/mediasource/media_source.cc): an unambiguous
/// H.264, AAC or MP3 codec string.
fn mp2t_codec_supported(c: &str) -> bool {
    match parse_codec_helper("", c) {
        Some(p) if !p.ambiguous => {
            if p.codec.is_audio() {
                matches!(p.codec, MCodec::Mpeg2Aac | MCodec::Mpeg4Aac | MCodec::Mpeg4XheAac | MCodec::Mp3)
            } else {
                p.video.is_some() && p.codec == MCodec::H264
            }
        }
        _ => false,
    }
}

/// `MediaSource::IsTypeSupportedInternal` with codec specificity enforced, as `isTypeSupported` asks it.
fn chromium_media_source(device: &Device, input: &str) -> bool {
    if input.is_empty() {
        return false;
    }
    let (ty, codecs) = blink_content_type(input);
    if ty.is_empty() {
        return false;
    }
    // ENABLE_MSE_MPEG2TS_STREAM_PARSER: Mime util has no `video/mp2t` (so canPlayType stays ""); each codec is
    // checked as an MP2T codec instead. The type is compared as written.
    if ty == "video/mp2t" && device.proprietary {
        let ascii: String = codecs.unwrap_or_default().chars().map(|c| if c.is_ascii() { c } else { '?' }).collect();
        return split_codecs(&ascii).iter().all(|c| mp2t_codec_supported(c));
    }
    // "MediaSource.isTypeSupported() returning true implies that HTMLMediaElement.canPlayType() will return
    // "maybe" or "probably"".
    if chromium_can_play(device, input) == Supports::Not {
        return false;
    }
    if !ty.is_ascii() {
        return false;
    }
    let list = match codecs {
        Some(c) if c.is_ascii() => split_codecs(&c),
        _ => Vec::new(),
    };
    stream_parser_supported(device, &ty.to_ascii_lowercase(), &list)
}

// ---- the three answers --------------------------------------------------------------------------
//
// EACH TAKES THE SCOPE BECAUSE THE ANSWER IS THE WORN DEVICE'S. They were pure functions
// of the input string over one compiled-in grid, so an engine wearing Opera 135 -- whose Linux build
// ships no proprietary H.264/AAC decoder -- told every page `canPlayType('video/mp4;
// codecs="avc1.42E01E"')` was `"probably"`. Measured over the four captures in this tree: 274
// answers Opera's own grid disagrees with, 16 Edge's, the largest single hard-coded difference in
// this goal. Every call site already held a scope; nothing had ever used it.
//
// R7 STILL STANDS, and a green codec gate does NOT mean playback works. This engine has no demuxer
// and no decoder, `readyState` is `HAVE_NOTHING` everywhere and `play()` rejects. `canPlayType` has
// always been the recorded browser's claim rather than a statement about this process; all that
// changes here is WHICH recorded browser's claim it is.

/// `HTMLMediaElement.canPlayType`: `""`, `"maybe"` or `"probably"` -- Chromium's `MimeUtil` over the worn
/// device ([`chromium_can_play`]).
///
/// "maybe" is a container that needs codecs and was given none, an ambiguous codec string (`avc1`,
/// `mp4a.40`, an H.264 level outside Table A-1) or an H.264 profile only some hardware decodes; "probably"
/// needs every codec to be valid for the container and decoded; one that is not makes the whole answer "",
/// which is why `video/mp4; codecs="avc1.42E01E, nonsense"` is "" and not "maybe".
pub fn can_play_type(scope: &mut v8::PinScope, input: &str) -> &'static str {
    match active_device(scope) {
        Some(d) => answer_can_play(&d, input),
        None => "",
    }
}

fn answer_can_play(device: &Device, input: &str) -> &'static str {
    match chromium_can_play(device, input) {
        Supports::Not => "",
        Supports::Maybe => "maybe",
        Supports::Probably => "probably",
    }
}

/// `MediaSource.isTypeSupported` -- Blink's `MediaSource::IsTypeSupportedInternal` over the worn device
/// ([`chromium_media_source`]).
///
/// Not the same question as `canPlayType`: MSE needs a byte-stream format it can parse, so most
/// containers want an explicit codec list (`video/mp4` alone is false) while the single-codec
/// containers do not (`audio/mpeg` alone is true), and its codec patterns are its own (`vp8.0` and the
/// ambiguous `avc1` are no MSE codec; `video/mp2t` is an MSE format `canPlayType` does not know).
pub fn media_source_supported(scope: &mut v8::PinScope, input: &str) -> bool {
    match active_device(scope) {
        Some(d) => chromium_media_source(&d, input),
        None => false,
    }
}

/// The reduced table's MSE answer, which `MediaCapabilities`' `media-source` question still reads
/// ([`answer_decoding`]).
fn answer_media_source(table: &Table, input: &str) -> bool {
    let Some(m) = parse_mime(input) else { return false };
    let Some(row) = table.get(&m.ty) else { return false };
    let Some(codecs) = m.codecs else { return row.base_ms };
    for c in &codecs {
        // An ambiguous codec is a no for MSE: a byte-stream parser has to be chosen up front, so
        // `video/mp4; codecs="avc1"` is false where `canPlayType` says "maybe". So is the legacy
        // `vp8.0` alias, which `canPlayType` accepts and MSE does not.
        match canonical_codec(c) {
            Some(k) if !k.ambiguous && !k.legacy && row.ids_ms.contains(&k.id) => {}
            _ => return false,
        }
    }
    true
}

/// `MediaCapabilities.decodingInfo`'s `supported`, for one `contentType`.
///
/// A FOURTH QUESTION, not a spelling of one of the three above. Measured 2026-09-18 over
/// 42 (contentType x MediaDecodingType) cells, on real Brave 151.1.93.138 AND stock Chrome 150 --
/// **56 of 56 answers identical**, so the rules below are the platform's and not a device's:
///
/// ```text
/// contentType                            canPlayType   MSE     decodingInfo(file)  (media-source)
/// video/mp4; codecs="avc1.42E01E"        probably      true    true                true
/// video/webm; codecs="vp8"               probably      true    true                true
/// video/webm; codecs="vp9"               probably      true    FALSE               FALSE
/// video/webm; codecs="vp09.00.10.08"     probably      true    true                true
/// video/mp4                              maybe         false   false               false
/// audio/mpeg                             probably      true    true                true
/// audio/ogg; codecs="flac"               probably      FALSE   TRUE                false
/// audio/wav; codecs="1"                  probably      FALSE   TRUE                false
/// video/mp4; codecs="hvc1.1.6.L93.B0"    ""            false   false               false
/// ```
///
/// Three facts come out of that table and none of them is guessable from the other three members:
///
///   * **`media-source` IS `MediaSource.isTypeSupported`** (the flac/wav rows are the proof: `file`
///     and `media-source` disagree for exactly the containers MSE has no byte-stream parser for).
///   * **`file` is `canPlayType == "probably"`** -- `"maybe"` is false, which is why `video/mp4` with
///     no codecs is false while `audio/mpeg` with no codecs is true.
///   * **the LEGACY VP9 SPELLING is ambiguous to MediaCapabilities and not to the other two.**
///     `codecs="vp9"` is `"probably"` to `canPlayType` and true to MSE, and false here, while `vp8`
///     -- a codec with no profiles to be unsure about -- is true in all three. Chromium's media-capability
///     path parses the codec string with ambiguity REJECTED, so a spelling that names no profile is not
///     an answerable question; `canPlayType`'s legacy codec map never asks.
///
/// This engine's recorded answers for the other three are untouched: [`mc_ambiguous`] is consulted
/// here and nowhere else.
pub fn decoding_supported(scope: &mut v8::PinScope, input: &str, media_source: bool) -> bool {
    match active_table(scope) {
        Some(t) => answer_decoding(&t, input, media_source),
        None => false,
    }
}

/// The codec spellings MediaCapabilities calls ambiguous although `canPlayType` accepts them: a
/// VP9 named with no profile/level, in either legacy spelling. See [`decoding_supported`].
fn mc_ambiguous(k: &Codec) -> bool {
    k.ambiguous || k.id == "vp9"
}

fn answer_decoding(table: &Table, input: &str, media_source: bool) -> bool {
    if media_source {
        // The same question MSE answers, plus the ambiguity rule the measurement adds.
        let Some(m) = parse_mime(input) else { return false };
        if let Some(codecs) = &m.codecs {
            if codecs.iter().any(|c| canonical_codec(c).as_ref().is_some_and(mc_ambiguous)) {
                return false;
            }
        }
        return answer_media_source(table, input);
    }
    let Some(m) = parse_mime(input) else { return false };
    let Some(row) = table.get(&m.ty) else { return false };
    let Some(codecs) = m.codecs else { return row.base_video == "probably" };
    if row.base_video.is_empty() {
        // A container this device does not demux at all: `canPlayType` says "" whatever the codecs.
        return false;
    }
    codecs.iter().all(|c| match canonical_codec(c) {
        Some(k) => !mc_ambiguous(&k) && row.ids_video.contains(&k.id),
        None => false,
    })
}

/// `MediaRecorder.isTypeSupported`.
///
/// The empty type is TRUE: it means "pick whatever you record in by default", and the recording says
/// so for the bare `""` and for every `; codecs="..."` hung off an empty type.
pub fn recorder_supported(scope: &mut v8::PinScope, input: &str) -> bool {
    match active_table(scope) {
        Some(t) => answer_recorder(&t, input),
        None => false,
    }
}

fn answer_recorder(table: &Table, input: &str) -> bool {
    let Some(m) = parse_mime(input) else {
        let t = input.trim();
        // THE EMPTY TYPE IS THE DEVICE'S ANSWER TOO, not a constant: `isTypeSupported("")` means
        // "choose whatever you record in by default", and the grid records it under the empty
        // container. A device that recorded no such row says false, which is why this reads the
        // table rather than returning true outright.
        return (t.is_empty() || t.starts_with(';')) && table.get("").is_some_and(|r| r.base_mr);
    };
    let Some(row) = table.get(&m.ty) else { return false };
    let Some(codecs) = m.codecs else { return row.base_mr };
    if !row.base_mr {
        return false;
    }
    for c in &codecs {
        if row.mr_lenient {
            // The family match accepts a spelling because the container muxes that family -- but the
            // recording rejected a few spellings whose family it otherwise muxes (bare `hev1` where
            // `hev1.1.6.L93.B0` muxes), and the recording is the authority, so those are denied first.
            if row.mr_deny.contains(&c.to_ascii_lowercase()) {
                return false;
            }
            if !row.mr_family.contains(&family_of(c).to_ascii_lowercase()) {
                return false;
            }
            continue;
        }
        if row.mr_exact.contains(c) {
            continue;
        }
        // Not a spelling the sweep asked about. The parameterised families generalise -- the muxer
        // parses the profile string, so any spelling that parses under a family the container carries
        // is accepted, and one that does not parse is not. Everything else is matched literally.
        let fam = family_of(c);
        if fam == c.as_str() || !PARAMETERISED.contains(&fam) || !row.mr_exact.contains(fam) {
            return false;
        }
        if canonical_codec(c).is_none() {
            return false;
        }
    }
    true
}

// ---- the two ops that used to serve the JS-owned interfaces are GONE ----------------
//
// `op_codec_media_source_supported` and `op_codec_recorder_supported` existed because `MediaSource`
// and `MediaRecorder` were `dom/09k-media-rtc.js`'s interfaces and their `isTypeSupported` statics
// needed a way into this grammar from JS. Both interfaces are Rust tables now
// (`core/media/media_source.rs`, `core/media/media_recorder.rs`) and their rows call
// [`media_source_supported`] / [`recorder_supported`] directly, so the ops had no caller. The
// one-place property the comment here defended is unchanged -- it is the two functions above that
// hold it, not the ops that wrapped them. `canPlayType` never needed an op: it is a Rust row of
// `HTMLMediaElement`. This file declares no ops at all now.

/// `canPlayType`'s Rust body: the `HTMLMediaElement` row (`core/html/html_media_element.rs` names it).
pub(crate) fn cb_can_play_type(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let ty = args.get(0).to_rust_string_lossy(scope);
    let answer = can_play_type(scope, &ty);
    if let Some(s) = v8::String::new(scope, answer) {
        rv.set(s.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The grammar under ONE named device, reduced exactly as a running realm reduces it.
    ///
    /// These cases are the SHIPPED bundle's, because they were written against its recording; naming
    /// it here rather than reading whatever the process wears is what keeps them a statement about a
    /// device instead of a statement about an environment variable. `persona` is a dev-dependency of
    /// this crate, so the reduction under test is reached with no engine boot at all.
    fn shipped_grid() -> Arc<spec::CodecGrid> {
        persona::codecs::for_bundle("chrome-152-linux-20261016T1022", None).expect("the shipped bundle's codec grid derives")
    }
    fn shipped() -> Arc<Table> {
        table_for(&shipped_grid())
    }
    fn shipped_device() -> Arc<Device> {
        device_for(&shipped_grid()).expect("the shipped bundle is a device")
    }

    /// One bundle by id, out of the bundles directory.
    fn worn_grid(id: &str) -> Arc<spec::CodecGrid> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../plugins/korvus-persona/bundles")
            .join(id);
        persona::codecs::for_bundle(id, Some(&dir)).unwrap_or_else(|| panic!("{id}: the codec grid derives"))
    }
    fn worn_device(id: &str) -> Arc<Device> {
        device_for(&worn_grid(id)).unwrap_or_else(|| panic!("{id}: the grid is a device"))
    }

    /// Every bundle in the tree, sorted — derived from the directory, never a list.
    fn bundle_ids() -> Vec<String> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../plugins/korvus-persona/bundles");
        let mut out: Vec<String> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
            .filter_map(|e| e.ok())
            .filter(|e| e.path().join("manifest.toml").is_file())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        out.sort();
        out
    }

    /// The four cases `tests/fixtures/constant-answers.js` asks of `canPlayType`, and the shapes the
    /// deleted `dom/00c-media-codecs.js` documented. Every expectation here is the recorded grid's,
    /// reached through the grammar rather than looked up.
    #[test]
    fn can_play_type_matches_the_recording() {
        let d = &shipped_device();
        let can_play_type = |s: &str| answer_can_play(d, s);
        assert_eq!(can_play_type("video/mp4"), "maybe");
        assert_eq!(can_play_type(r#"video/mp4; codecs="avc1.42E01E""#), "probably");
        assert_eq!(can_play_type(r#"video/mp4; codecs="avc1""#), "maybe");
        assert_eq!(can_play_type(r#"video/mp4; codecs="avc1.42E01E, nonsense""#), "");
        assert_eq!(can_play_type(r#"video/webm; codecs="vp8""#), "probably");
        assert_eq!(can_play_type(r#"video/webm; codecs="vp8.0""#), "probably");
        assert_eq!(can_play_type(r#"video/webm; codecs="""#), "maybe");
        assert_eq!(can_play_type("audio/mpeg"), "probably");
        assert_eq!(can_play_type(r#"audio/ogg; codecs="vorbis""#), "probably");
        assert_eq!(can_play_type("application/x-not-a-type"), "");
        assert_eq!(can_play_type("x"), "");
        assert_eq!(can_play_type("video/mp2t"), "");
        assert_eq!(can_play_type("VIDEO/WEBM"), "maybe");
    }

    #[test]
    fn media_source_is_the_other_question() {
        let d = &shipped_device();
        let media_source_supported = |s: &str| chromium_media_source(d, s);
        assert!(!media_source_supported("x"));
        assert!(media_source_supported(r#"video/mp4; codecs="avc1.42E01E""#));
        assert!(media_source_supported(r#"video/webm; codecs="vp8""#));
        assert!(media_source_supported("audio/mpeg"));
        assert!(!media_source_supported("application/x-not-a-type"));
        // MSE needs a codec list where canPlayType does not, and rejects the ambiguous + legacy
        // spellings canPlayType accepts.
        assert!(!media_source_supported("video/mp4"));
        assert!(!media_source_supported(r#"video/mp4; codecs="avc1""#));
        assert!(!media_source_supported(r#"video/webm; codecs="vp8.0""#));
        // `video/mp2t` is a byte-stream format canPlayType does not know; its codecs are checked as MP2T codecs.
        assert!(media_source_supported("video/mp2t"));
        assert!(media_source_supported(r#"video/mp2t; codecs="avc1.4D401E, mp4a.40.2""#));
        assert!(!media_source_supported(r#"video/mp2t; codecs="vp8""#));
        // An MSE pattern that canPlayType rejects: an invalid VP9 level is no codec to either.
        assert!(!media_source_supported(r#"video/webm; codecs="vp09.00.15.08""#));
        // The containers canPlayType answers "maybe" for have no MSE byte-stream parser.
        assert!(!media_source_supported(r#"audio/x-m4a; codecs="mp4a.40.2""#));
    }

    #[test]
    fn recorder_reads_the_family_where_the_muxer_does() {
        let t = &shipped();
        let recorder_supported = |s: &str| answer_recorder(t, s);
        assert!(!recorder_supported("x"));
        assert!(recorder_supported("video/webm;codecs=vp8"));
        assert!(recorder_supported("audio/webm;codecs=opus"));
        assert!(!recorder_supported("application/x-not-a-type"));
        // The empty type means "choose a default".
        assert!(recorder_supported(""));
        // WebM's muxer never parses the parameters; MP4's does.
        assert!(recorder_supported(r#"video/webm; codecs="avc1.ZZZZZZ""#));
        assert!(recorder_supported(r#"video/webm; codecs="AVC1.42E01E""#));
        assert!(!recorder_supported(r#"video/mp4; codecs="avc1.42E0""#));
        assert!(recorder_supported(r#"video/mp4; codecs="avc1""#));
        assert!(!recorder_supported(r#"video/mp4; codecs="Opus""#));
        // `vp09.00.10.08`'s family is `vp09`; MediaRecorder's WebM list has `vp9`.
        assert!(!recorder_supported(r#"video/webm; codecs="vp09.00.10.08""#));
    }

    /// THE GRAMMAR IS ONE ALGORITHM AND THE GRID IS THE DEVICE'S.
    ///
    /// The same three questions, asked of every bundle in the tree, must produce that bundle's own
    /// recorded answer -- and a grid that records no proprietary decoder must answer otherwise, or the
    /// seam proves nothing. Opera 135's Linux build was that grid while it was a bundle here; Chrome is
    /// the one persona since 2026-10-16, so the test strips Chrome's grid to it. This is the reduction
    /// half of that claim, with no engine in it; `tests/codec_grid_live.rs` is the live half, over
    /// all 9,104 recorded answers of each device.
    #[test]
    fn every_device_answers_its_own_grid_and_a_grid_without_the_decoder_does_not() {
        let ids = bundle_ids();
        assert!(!ids.is_empty(), "no persona bundle in the tree");
        const H264: &str = r#"video/mp4; codecs="avc1.42E01E""#;
        let mut answers: Vec<(String, &'static str, bool)> = Vec::new();
        for id in &ids {
            let d = worn_device(id);
            answers.push((id.clone(), answer_can_play(&d, H264), chromium_media_source(&d, H264)));
        }
        // Non-vacuous: the same grid without the proprietary containers and spellings answers H.264 with "".
        let worn = worn_grid(&ids[0]);
        let proprietary = |c: &String| ["avc1", "avc3", "hev1", "hvc1", "mp4a"].iter().any(|p| c.starts_with(p));
        let free = spec::CodecGrid {
            capture_id: format!("{}-no-proprietary", worn.capture_id),
            containers: worn
                .containers
                .iter()
                .filter(|(c, _)| !["audio/aac", "audio/x-m4a", "video/x-m4v", "video/3gpp"].contains(&c.as_str()))
                .map(|(c, row)| {
                    let mut row = row.clone();
                    for list in [&mut row.video, &mut row.audio, &mut row.ms, &mut row.mr] {
                        list.retain(|s| !proprietary(s));
                    }
                    (c.clone(), row)
                })
                .collect(),
            asked: worn.asked.clone(),
        };
        let free = derive_device(&free).expect("the stripped grid is still a device");
        assert_eq!(answers[0].1, "probably", "{}: {H264}", ids[0]);
        assert_eq!(answer_can_play(&free, H264), "", "a grid with no H.264 decoder: {H264}");
        assert!(!chromium_media_source(&free, H264), "a grid with no H.264 decoder: MediaSource {H264}");
        // And each device's answer is ITS OWN capture's, not a neighbour's.
        for (id, cp, ms) in &answers {
            let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../plugins/korvus-persona/bundles")
                .join(id);
            let grid = persona::codecs::for_bundle(id, Some(&dir)).unwrap();
            let row = &grid.containers["video/mp4"];
            let recorded = row.video.iter().any(|c| c == "avc1.42E01E");
            assert_eq!(*cp, if recorded { "probably" } else { "" }, "{id}: canPlayType({H264})");
            assert_eq!(
                *ms,
                row.ms.iter().any(|c| c == "avc1.42E01E"),
                "{id}: MediaSource.isTypeSupported({H264})"
            );
        }
    }

    /// A realm with NO device claims nothing -- which is what an engine with no demuxer, no decoder
    /// and no recorded browser to quote should say (ruling R7). Asserted over an EMPTY table and an EMPTY
    /// grid rather than by booting a persona-less runtime, because those are the exact states the
    /// wrappers short-circuit on.
    #[test]
    fn a_device_less_realm_claims_no_codec_at_all() {
        assert!(derive_device(&spec::CodecGrid::default()).is_none(), "a grid that records nothing is no device");
        let empty: Table = Table::new();
        assert!(!answer_media_source(&empty, r#"video/mp4; codecs="avc1.42E01E""#));
        assert!(!answer_recorder(&empty, "video/webm;codecs=vp8"));
        // Even the empty type, whose `true` is the shipped device's recorded answer and not a law.
        assert!(!answer_recorder(&empty, ""));
    }

    /// The algorithm generalises past the sweep: a spelling nobody recorded gets Chromium's answer. Each
    /// expectation is stock Chrome 150's (`tests/fixtures/media-mime/chrome-150.json` records these rules on
    /// sibling spellings: `avc1.42E02B` "maybe", `avc1.6E001F` "probably").
    #[test]
    fn unrecorded_spellings_are_computed() {
        let d = &shipped_device();
        let can_play_type = |s: &str| answer_can_play(d, s);
        // level 0x2B (4.3) is not in Table A-1: the string is ambiguous, so "maybe".
        assert_eq!(can_play_type(r#"video/mp4; codecs="avc1.64002B""#), "maybe");
        assert_eq!(can_play_type(r#"video/mp4; codecs="avc1.4D4028""#), "probably");
        // High 10 (0x6E) with FFmpeg's built-in H.264 decoder: "probably".
        assert_eq!(can_play_type(r#"video/mp4; codecs="avc1.6E001E""#), "probably");
        // Extended (0x58) without constraint_set0 is "only supported on some hardware": "maybe".
        assert_eq!(can_play_type(r#"video/mp4; codecs="avc1.58001E""#), "maybe");
        // vp09 needs its four mandatory fields, a known profile, level and bit depth.
        assert_eq!(can_play_type(r#"video/webm; codecs="vp09.01.20.10""#), "probably");
        assert_eq!(can_play_type(r#"video/webm; codecs="vp09.00.10""#), "");
        assert_eq!(can_play_type(r#"video/webm; codecs="vp09.00.15.08""#), "");
        // mp4a.40.23 is not in the codec map; mp4a.40.42 is, and Linux has no xHE-AAC decoder.
        assert_eq!(can_play_type(r#"audio/mp4; codecs="mp4a.40.23""#), "");
        assert_eq!(can_play_type(r#"audio/mp4; codecs="mp4a.40.42""#), "");
        assert_eq!(can_play_type(r#"audio/mp4; codecs="mp4a.40.5""#), "probably");
        assert_eq!(can_play_type(r#"audio/mp4; codecs="mp4a.40""#), "maybe");
    }

    /// THE CONTAINERS THE SWEEP NEVER ASKED ABOUT (a device-check script's `acma`, ledger N20): Chromium registers them,
    /// so they answer as Chrome 150 does instead of `""`.
    #[test]
    fn chromium_registers_containers_the_grid_never_recorded() {
        let d = &shipped_device();
        for c in ["audio/x-m4a", "video/x-m4v", "audio/matroska", "video/matroska", "audio/x-mpegurl"] {
            assert_eq!(answer_can_play(d, c), "maybe", "{c}");
        }
        assert_eq!(answer_can_play(d, "audio/x-m4a;"), "maybe");
        assert_eq!(answer_can_play(d, r#"audio/x-m4a; codecs="mp4a.40.2""#), "probably");
        assert_eq!(answer_can_play(d, r#"video/x-m4v; codecs="avc1.42E01E, mp4a.40.2""#), "probably");
        // HLS takes MPEG-4 AAC and not MPEG-2 AAC.
        assert_eq!(answer_can_play(d, r#"audio/x-mpegurl; codecs="mp4a.67""#), "");
        // Chromium has no `audio/3gpp` (Edge 151 patched one in, which its grid said while it was a bundle here).
        assert_eq!(answer_can_play(d, "audio/3gpp"), "");
    }

    /// Blink's `ContentType` parse and the number grammar under the codec parsers, on the strings stock
    /// Chrome 150 answered (`tests/fixtures/media-mime/chrome-150.json`).
    #[test]
    fn the_content_type_and_number_grammar_is_blinks() {
        let d = &shipped_device();
        let can_play_type = |s: &str| answer_can_play(d, s);
        assert_eq!(can_play_type(r#"video/webm; codecs="vp8,,opus""#), "");
        assert_eq!(can_play_type(r#"video/webm; codecs="vp8"; codecs="opus""#), "probably");
        assert_eq!(can_play_type(r#"video/webm; foo="a;b"; codecs="vp8""#), "probably");
        assert_eq!(can_play_type(r#"video/webm codecs="vp8""#), "");
        assert_eq!(can_play_type("vidéo/webm"), "");
        assert_eq!(can_play_type("video/webm; codecs=\"vp8é\""), "maybe");
        assert_eq!(can_play_type(r#"video/mp4; codecs="AVC1.42E01E""#), "");
        assert_eq!(can_play_type(r#"video/mp4; codecs="avc1.42e01e""#), "probably");
        assert_eq!(can_play_type(r#"video/mp4; codecs="avc1.0x42E0""#), "");
        assert_eq!(can_play_type(r#"video/webm; codecs="vp09.+0.10.08""#), "probably");
        assert_eq!(can_play_type(r#"video/webm; codecs="vp09.-0.10.08""#), "probably");
        assert_eq!(can_play_type(r#"audio/mpeg; codecs="mp3""#), "probably");
        assert_eq!(can_play_type(r#"audio/flac; codecs="flac""#), "");
        assert_eq!(chromium_number("0x1F", 16, false), Some(31));
        assert_eq!(chromium_number(" 1", 10, true), None);
        assert_eq!(chromium_number("-1", 10, false), None);
        assert_eq!(chromium_number("4294967296", 10, false), None);
    }
}
