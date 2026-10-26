//! Reader over the captured value profile (`the persona crate's embedded values (`persona::SHIPPED_VALUES`)`) —
//! the axis-④ reference. The profile is the real Chrome-151 capture: `nav`/`screen`/`win`/`intl`/…
//! sections of scalars a page reads (navigator.hardwareConcurrency, screen.width, …).
//!
//! Only members with a known `(iface, member) -> profile-path` mapping are graded on axis ④; every
//! other member returns `None` from [`Profile::expected`] and is simply not value-graded (a method,
//! a value with no captured reference, an interface the profile doesn't cover).

use std::path::Path;

use serde_json::Value;

/// The static `(iface, member) -> dotted profile path` table for value-bearing members. Covers the
/// obvious `nav`/`screen`/`win` scalars; extend as more captured references land.
const PROFILE_PATHS: &[(&str, &str, &str)] = &[
    // Navigator (nav.*)
    ("Navigator", "hardwareConcurrency", "nav.hardwareConcurrency"),
    ("Navigator", "deviceMemory", "nav.deviceMemory"),
    ("Navigator", "userAgent", "nav.userAgent"),
    ("Navigator", "appVersion", "nav.appVersion"),
    ("Navigator", "appName", "nav.appName"),
    ("Navigator", "appCodeName", "nav.appCodeName"),
    ("Navigator", "product", "nav.product"),
    ("Navigator", "productSub", "nav.productSub"),
    ("Navigator", "vendor", "nav.vendor"),
    ("Navigator", "vendorSub", "nav.vendorSub"),
    ("Navigator", "platform", "nav.platform"),
    ("Navigator", "language", "nav.language"),
    ("Navigator", "languages", "nav.languages"),
    ("Navigator", "maxTouchPoints", "nav.maxTouchPoints"),
    ("Navigator", "pdfViewerEnabled", "nav.pdfViewerEnabled"),
    ("Navigator", "doNotTrack", "nav.doNotTrack"),
    ("Navigator", "cookieEnabled", "nav.cookieEnabled"),
    ("Navigator", "onLine", "nav.onLine"),
    ("Navigator", "webdriver", "nav.webdriver"),
    // Screen (screen.*)
    ("Screen", "width", "screen.width"),
    ("Screen", "height", "screen.height"),
    ("Screen", "availWidth", "screen.availWidth"),
    ("Screen", "availHeight", "screen.availHeight"),
    ("Screen", "colorDepth", "screen.colorDepth"),
    ("Screen", "pixelDepth", "screen.pixelDepth"),
    ("Screen", "availLeft", "screen.availLeft"),
    ("Screen", "availTop", "screen.availTop"),
    // Window (win.*)
    ("Window", "innerWidth", "win.innerWidth"),
    ("Window", "innerHeight", "win.innerHeight"),
    ("Window", "outerWidth", "win.outerWidth"),
    ("Window", "outerHeight", "win.outerHeight"),
    ("Window", "devicePixelRatio", "win.devicePixelRatio"),
    ("Window", "screenX", "win.screenX"),
    ("Window", "screenY", "win.screenY"),
    // NavigatorUAData scalars (the sync client-hints surface; high-entropy values are captured under
    // nav.uaHighEntropy). The async getHighEntropyValues() fields are graded via the same section.
    ("NavigatorUAData", "mobile", "nav.uaHighEntropy.mobile"),
    ("NavigatorUAData", "platform", "nav.uaHighEntropy.platform"),
];

/// The captured value profile.
#[derive(Debug, Clone)]
pub struct Profile {
    root: Value,
}

impl Profile {
    pub fn from_json(json: &str) -> serde_json::Result<Profile> {
        Ok(Profile { root: serde_json::from_str(json)? })
    }

    pub fn from_path(path: &Path) -> std::io::Result<Profile> {
        let json = std::fs::read_to_string(path)?;
        Profile::from_json(&json).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// Resolve a dotted path (`"nav.hardwareConcurrency"`) into the profile JSON.
    pub fn get_path(&self, dotted: &str) -> Option<&Value> {
        let mut cur = &self.root;
        for seg in dotted.split('.') {
            cur = cur.get(seg)?;
        }
        Some(cur)
    }

    /// The expected profile value for a graded `(iface, member)`, or `None` when the member is not in
    /// the value table (axis ④ not graded) or the path is absent from this profile.
    pub fn expected(&self, iface: &str, member: &str) -> Option<Value> {
        let path = PROFILE_PATHS
            .iter()
            .find(|(i, m, _)| *i == iface && *m == member)
            .map(|(_, _, p)| *p)?;
        self.get_path(path).cloned()
    }

    /// Whether `(iface, member)` is in the value table at all (mapped), regardless of whether this
    /// profile carries the path.
    pub fn is_mapped(iface: &str, member: &str) -> bool {
        PROFILE_PATHS.iter().any(|(i, m, _)| *i == iface && *m == member)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROFILE: &str = r#"{
      "nav": { "hardwareConcurrency": 10, "deviceMemory": 8, "platform": "MacIntel",
               "languages": ["en-US", "en"], "doNotTrack": null },
      "screen": { "width": 1512, "colorDepth": 30 },
      "win": { "innerWidth": 1440, "devicePixelRatio": 2 }
    }"#;

    #[test]
    fn maps_the_obvious_scalars() {
        let p = Profile::from_json(PROFILE).expect("parse");
        assert_eq!(p.expected("Navigator", "hardwareConcurrency"), Some(Value::from(10)));
        assert_eq!(p.expected("Navigator", "platform"), Some(Value::from("MacIntel")));
        assert_eq!(p.expected("Navigator", "languages"), Some(serde_json::json!(["en-US", "en"])));
        assert_eq!(p.expected("Navigator", "doNotTrack"), Some(Value::Null));
        assert_eq!(p.expected("Screen", "width"), Some(Value::from(1512)));
        assert_eq!(p.expected("Window", "devicePixelRatio"), Some(Value::from(2)));
    }

    #[test]
    fn maps_navigator_ua_data_scalars() {
        // Phase A: NavigatorUAData.{mobile,platform} come from the captured high-entropy UA data.
        let p = Profile::from_json(
            r#"{ "nav": { "uaHighEntropy": { "mobile": false, "platform": "macOS" } } }"#,
        )
        .unwrap();
        assert_eq!(p.expected("NavigatorUAData", "mobile"), Some(Value::from(false)));
        assert_eq!(p.expected("NavigatorUAData", "platform"), Some(Value::from("macOS")));
        assert!(Profile::is_mapped("NavigatorUAData", "mobile"));
    }

    #[test]
    fn unmapped_member_is_none_even_when_present() {
        let p = Profile::from_json(PROFILE).expect("parse");
        // Not in the table at all -> None (not graded), even though it could be resolved.
        assert_eq!(p.expected("Navigator", "somethingElse"), None);
        assert!(!Profile::is_mapped("Navigator", "somethingElse"));
        assert!(Profile::is_mapped("Navigator", "hardwareConcurrency"));
        // Mapped, but this trimmed profile lacks the path -> None (can't grade what we didn't capture).
        assert_eq!(p.expected("Navigator", "userAgent"), None);
    }
}
