//! The device's `RTCRtpReceiver.getCapabilities(kind)` / `RTCRtpSender.getCapabilities(kind)` answers --
//! `webrtc-capabilities.json`.
//!
//! WHICH CODECS A LIST HOLDS IS THE BUILD'S AND THE DEVICE'S: libwebrtc's decoder factory for the receiver, its
//! encoder factory for the sender, hardware codecs included. Measured on the four installed browsers, headed under
//! Xvfb as the offer beside it was (`webrtc-offer.json`): Chrome 150 lists 19 receive and 13 send video codecs,
//! Opera 135's Linux build 11 and 7 (no H.264); the receiver's list is not the offer's (it adds decode-only
//! codecs) and the sender's is a different subset in a different order, so neither is derived from the offer.
//! The engine answered `null` for every kind, where every Chromium answers these lists -- and an interactive-widget challenge
//! challenge frame reads both. Read through the persona's OWN bundle exactly as [`crate::webrtc_offer`] is; a
//! bundle with no such artifact is `None`, and the engine then answers `null`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

/// The shipped bundle's answers, embedded beside its other artifacts: a build with no bundle selected reads no
/// file.
pub const SHIPPED_WEBRTC_CAPABILITIES: &str =
    include_str!("../bundles/chrome-152-linux-20261016T1022/webrtc-capabilities.json");

/// The four lists the file holds, under the names the engine looks them up by.
pub const SIDES: [&str; 4] = ["RTCRtpReceiver.audio", "RTCRtpReceiver.video", "RTCRtpSender.audio", "RTCRtpSender.video"];

/// One side's lists as the file keeps them: each codec and header extension one JSON object per entry, so an
/// edit is a reviewable diff (tools/parity/tests/test_bundle_json_is_reviewable.py).
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Lists {
    codecs: Vec<String>,
    header_extensions: Vec<String>,
}

#[derive(serde::Deserialize)]
struct CapabilitiesFile {
    capabilities: BTreeMap<String, Lists>,
}

/// Parse a `webrtc-capabilities.json`: each side's `RTCRtpCapabilities` dictionary as JSON text, `codecs` before
/// `headerExtensions` and each entry's members in the order the file keeps them (sorted: a WebIDL dictionary's).
pub fn derive(json: &str) -> Result<spec::WebrtcCapabilities, String> {
    let f: CapabilitiesFile = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let mut by_side_kind = BTreeMap::new();
    for side in SIDES {
        let lists = f.capabilities.get(side).ok_or_else(|| format!("no {side} lists"))?;
        for entry in lists.codecs.iter().chain(&lists.header_extensions) {
            let v: serde_json::Value = serde_json::from_str(entry).map_err(|e| format!("{side}: {entry}: {e}"))?;
            if !v.is_object() {
                return Err(format!("{side}: {entry} is not an object"));
            }
        }
        let text = format!("{{\"codecs\":[{}],\"headerExtensions\":[{}]}}", lists.codecs.join(","),
                           lists.header_extensions.join(","));
        by_side_kind.insert(side.to_string(), text);
    }
    Ok(spec::WebrtcCapabilities { by_side_kind })
}

type Cache = Mutex<BTreeMap<String, Option<Arc<spec::WebrtcCapabilities>>>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// The answers of the bundle in `dir` (`None` = the embedded one), read at most once per process.
pub fn for_bundle(dir: Option<&std::path::Path>) -> Option<Arc<spec::WebrtcCapabilities>> {
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

fn load(capture_id: &str, dir: Option<&std::path::Path>) -> Option<spec::WebrtcCapabilities> {
    let (json, source): (std::borrow::Cow<'static, str>, String) = match dir {
        None => (SHIPPED_WEBRTC_CAPABILITIES.into(), "<embedded webrtc-capabilities>".to_string()),
        Some(dir) => {
            let path = crate::surface::artifact_in(dir, "webrtc-capabilities")?;
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
            eprintln!("[korvus] persona {capture_id}: {source} does not parse as getCapabilities answers: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every bundle carries its answers, all four sides parse into JSON the engine can hand a page, and the
    /// embedded copy is the shipped bundle's file. The codecs are not asserted: they are the measurement's.
    #[test]
    fn every_bundle_has_capabilities_and_the_embedded_one_is_the_shipped_file() {
        let bundles = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("bundles");
        let mut seen = 0;
        for entry in std::fs::read_dir(bundles).unwrap() {
            let dir = entry.unwrap().path();
            if !dir.join("manifest.toml").exists() {
                continue;
            }
            let id = dir.file_name().unwrap().to_string_lossy().to_string();
            let path = crate::surface::artifact_in(&dir, "webrtc-capabilities")
                .unwrap_or_else(|| panic!("{id}: the manifest names no `webrtc-capabilities` artifact"));
            let text = std::fs::read_to_string(&path).unwrap();
            let c = derive(&text).unwrap_or_else(|e| panic!("{id}: webrtc-capabilities.json does not parse: {e}"));
            for side in SIDES {
                let v: serde_json::Value = serde_json::from_str(&c.by_side_kind[side]).unwrap();
                assert!(!v["codecs"].as_array().unwrap().is_empty(), "{id}: {side} lists no codec");
                assert!(!v["headerExtensions"].as_array().unwrap().is_empty(), "{id}: {side} lists no extension");
            }
            if id == crate::bundle::Bundle::parse(crate::BUNDLE_MANIFEST).unwrap().capture.id {
                assert_eq!(text, SHIPPED_WEBRTC_CAPABILITIES, "the embedded answers are not the shipped bundle's file");
            }
            seen += 1;
        }
        assert!(seen >= 1, "expected the shipped bundle at least, saw {seen}");
    }
}
