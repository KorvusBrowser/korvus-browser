//! The device's own SDP offer -- `webrtc-offer.json` -- reduced to its receive-only
//! audio and video sections.
//!
//! WHICH CODECS A SECTION LISTS IS THE BUILD'S, like the codec grid [`crate::codecs`] reads. Measured on the
//! four installed browsers for `createOffer({offerToReceiveAudio: 1, offerToReceiveVideo: 1})`: Chrome 150
//! writes 200 lines; Brave 151 and Edge 151 244, Chromium 151 adding RTCP XR (`a=rtcp-xr:rcvr-rtt=all` and an
//! `rrtr` feedback line per payload type) and numbering its video payload types differently; Opera 135 156,
//! its Linux build offering no H.264. The engine wrote Chrome 150's sections for every persona, from two
//! files compiled into `core/`. Read through the persona's OWN bundle exactly as [`crate::computed_style`]
//! is; a bundle with no such artifact is `None`, and the engine then refuses an offer with a media section.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

/// The shipped bundle's offer, embedded beside its other artifacts: a build with no bundle selected reads no
/// file.
pub const SHIPPED_WEBRTC_OFFER: &str = include_str!("../bundles/chrome-152-linux-20261016T1022/webrtc-offer.json");

/// The file keeps the offer one SDP line per entry, so an edit to it is a reviewable diff
/// (tools/parity/tests/test_bundle_json_is_reviewable.py); each line ends in CRLF in the SDP.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct OfferFile {
    sdp_lines: Vec<String>,
}

/// The offer's text, as the device wrote it.
pub fn sdp_of(json: &str) -> Result<String, String> {
    let f: OfferFile = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(f.sdp_lines.iter().map(|l| format!("{l}\r\n")).collect())
}

/// Parse a `webrtc-offer.json`: its offer's `m=audio` and `m=video` sections, each as written.
pub fn derive(json: &str) -> Result<spec::WebrtcOffer, String> {
    let sdp = sdp_of(json)?;
    let section = |kind: &str| -> Result<String, String> {
        let at = format!("{kind} ");
        sdp.split("\r\nm=")
            .skip(1)
            .find(|s| s.starts_with(&at))
            .map(|s| format!("m={}", s.trim_end_matches("\r\n")))
            .ok_or_else(|| format!("the offer has no m={kind} section"))
    };
    Ok(spec::WebrtcOffer { audio: section("audio")?, video: section("video")? })
}

type Cache = Mutex<BTreeMap<String, Option<Arc<spec::WebrtcOffer>>>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// The offer of the bundle in `dir` (`None` = the embedded one), read at most once per process.
pub fn for_bundle(dir: Option<&std::path::Path>) -> Option<Arc<spec::WebrtcOffer>> {
    let key = dir.map(|d| d.display().to_string()).unwrap_or_default();
    if let Ok(c) = cache().lock() {
        if let Some(hit) = c.get(&key) {
            return hit.clone();
        }
    }
    let id = dir.and_then(|d| d.file_name()).map(|n| n.to_string_lossy().to_string());
    let built = load(id.as_deref().unwrap_or("<embedded>"), dir).map(Arc::new);
    if let Ok(mut c) = cache().lock() {
        c.insert(key, built.clone());
    }
    built
}

fn load(capture_id: &str, dir: Option<&std::path::Path>) -> Option<spec::WebrtcOffer> {
    let (json, source): (std::borrow::Cow<'static, str>, String) = match dir {
        None => (SHIPPED_WEBRTC_OFFER.into(), "<embedded webrtc-offer>".to_string()),
        Some(dir) => {
            let path = crate::surface::artifact_in(dir, "webrtc-offer")?;
            match std::fs::read_to_string(&path) {
                Ok(s) => (s.into(), path.display().to_string()),
                Err(e) => {
                    eprintln!("[korvus] persona {capture_id}: cannot read {}: {e}", path.display());
                    return None;
                }
            }
        }
    };
    match derive(&json) {
        Ok(v) => Some(v),
        Err(e) => {
            eprintln!("[korvus] persona {capture_id}: {source} does not parse as an offer: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every bundle carries its offer, both sections parse, each is ONE section with the lines the engine
    /// writes its own values into, and the embedded copy is the shipped bundle's file. The codecs are not
    /// asserted: they are the measurement's.
    #[test]
    fn every_bundle_has_an_offer_and_the_embedded_one_is_the_shipped_file() {
        let bundles = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("bundles");
        let mut seen = 0;
        for entry in std::fs::read_dir(bundles).unwrap() {
            let dir = entry.unwrap().path();
            if !dir.join("manifest.toml").exists() {
                continue;
            }
            let id = dir.file_name().unwrap().to_string_lossy().to_string();
            let path = crate::surface::artifact_in(&dir, "webrtc-offer")
                .unwrap_or_else(|| panic!("{id}: the manifest names no `webrtc-offer` artifact"));
            let text = std::fs::read_to_string(&path).unwrap();
            let o = derive(&text).unwrap_or_else(|e| panic!("{id}: webrtc-offer.json does not parse: {e}"));
            for (kind, s) in [("audio", &o.audio), ("video", &o.video)] {
                assert!(s.starts_with(&format!("m={kind} 9 UDP/TLS/RTP/SAVPF ")), "{id}: {kind}: {s:.60}");
                assert_eq!(s.matches("\r\nm=").count(), 0, "{id}: {kind} runs into the next section");
                assert!(!s.ends_with("\r\n"), "{id}: {kind} keeps the offer's last line break");
                for line in ["a=ice-ufrag:", "a=ice-pwd:", "a=fingerprint:", "a=mid:", "a=recvonly"] {
                    assert_eq!(s.split("\r\n").filter(|l| l.starts_with(line)).count(), 1, "{id}: {kind}: {line}");
                }
            }
            if id == crate::bundle::Bundle::parse(crate::BUNDLE_MANIFEST).unwrap().capture.id {
                assert_eq!(text, SHIPPED_WEBRTC_OFFER, "the embedded offer is not the shipped bundle's file");
            }
            seen += 1;
        }
        assert!(seen >= 1, "expected the shipped bundle at least, saw {seen}");
    }
}
