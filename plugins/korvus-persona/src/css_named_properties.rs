//! The names a `CSSStyleDeclaration`'s named-property enumerator lists on the worn device —
//! `css-named-properties.json` — in the browser's own order.
//!
//! WHICH NAMES EXIST is the build's table: Blink's `NamedPropertyEnumerator` lists the JS name of every
//! web-exposed CSS property and alias, sorted by code unit -- Chrome 150 744 (151 `webkit*`, 9 `epub*`), the
//! Chromium-151 browsers 745. A declaration answered none of them as its own (`'accentColor' in el.style` was
//! false) and a computed one enumerated its dashed computed names instead, where a bot-challenge frame
//! enumerates a computed style and reads every name. Read through the persona's OWN bundle exactly as
//! [`crate::computed_style`] is; a bundle with no such artifact is `None`, and the engine keeps its own table.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

/// The shipped bundle's list, embedded beside its other artifacts: a build with no bundle selected reads no file.
pub const SHIPPED_CSS_NAMED_PROPERTIES: &str =
    include_str!("../bundles/chrome-152-linux-20261016T1022/css-named-properties.json");

#[derive(serde::Deserialize)]
struct NamedPropertiesFile {
    names: Vec<String>,
}

/// Parse a `css-named-properties.json`.
pub fn derive(json: &str) -> Result<Vec<String>, serde_json::Error> {
    Ok(serde_json::from_str::<NamedPropertiesFile>(json)?.names)
}

type Cache = Mutex<BTreeMap<String, Option<Arc<Vec<String>>>>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// The list of the bundle in `dir` (`None` = the embedded one), read at most once per process.
pub fn for_bundle(dir: Option<&std::path::Path>) -> Option<Arc<Vec<String>>> {
    let key = dir.map(|d| d.display().to_string()).unwrap_or_default();
    if let Ok(c) = cache().lock() {
        if let Some(hit) = c.get(&key) {
            return hit.clone();
        }
    }
    let id = dir.and_then(|d| d.file_name()).map(|n| n.to_string_lossy().to_string());
    let built = load(id.as_deref().unwrap_or("<embedded>"), dir).filter(|v| !v.is_empty()).map(Arc::new);
    if let Ok(mut c) = cache().lock() {
        c.insert(key, built.clone());
    }
    built
}

fn load(capture_id: &str, dir: Option<&std::path::Path>) -> Option<Vec<String>> {
    let (json, source): (std::borrow::Cow<'static, str>, String) = match dir {
        None => (SHIPPED_CSS_NAMED_PROPERTIES.into(), "<embedded css-named-properties>".to_string()),
        Some(dir) => {
            let path = crate::surface::artifact_in(dir, "css-named-properties")?;
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
            eprintln!("[korvus] persona {capture_id}: {source} does not parse as a named-property list: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every bundle carries the list, it parses with no duplicate and no index, and the embedded copy is the
    /// shipped bundle's file. The counts are not asserted: they are the measurement's.
    #[test]
    fn every_bundle_has_a_named_property_list_and_the_embedded_one_is_the_shipped_file() {
        let bundles = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("bundles");
        let mut seen = 0;
        for entry in std::fs::read_dir(bundles).unwrap() {
            let dir = entry.unwrap().path();
            if !dir.join("manifest.toml").exists() {
                continue;
            }
            let id = dir.file_name().unwrap().to_string_lossy().to_string();
            let path = crate::surface::artifact_in(&dir, "css-named-properties")
                .unwrap_or_else(|| panic!("{id}: the manifest names no `css-named-properties` artifact"));
            let text = std::fs::read_to_string(&path).unwrap();
            let v = derive(&text).unwrap_or_else(|e| panic!("{id}: css-named-properties.json does not parse: {e}"));
            let uniq: std::collections::BTreeSet<&String> = v.iter().collect();
            assert_eq!(uniq.len(), v.len(), "{id}: a name is listed twice");
            assert!(v.iter().all(|n| !n.bytes().all(|b| b.is_ascii_digit())), "{id}: an index is listed");
            if id == crate::bundle::Bundle::parse(crate::BUNDLE_MANIFEST).unwrap().capture.id {
                assert_eq!(text, SHIPPED_CSS_NAMED_PROPERTIES, "the embedded list is not the shipped bundle's file");
            }
            seen += 1;
        }
        assert!(seen >= 1, "expected the shipped bundle at least, saw {seen}");
    }
}
