//! The worn device's platform-theme geometry — `theme.json` — as [`spec::ThemeProfile`].
//!
//! A classic scrollbar's thickness and a drop-down's theme paddings are facts about the PLATFORM a
//! browser draws with, not about any font and not about the engine. The capture rig reads them back
//! off layout (`capture/collect.js`, `layoutMetrics`) and `capture/emit.py --role theme` writes them
//! into the bundle; this is the reader, through the persona's OWN bundle exactly as
//! [`crate::codecs`] is, so swapping `KORVUS_BUNDLE` swaps the theme with everything else.
//!
//! A theme that is missing or malformed is `None`, never a panic and never a compiled-in number: the
//! engine then lays out with no classic scrollbar and no theme padding, and the reason is printed
//! once on stderr.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use spec::ThemeProfile;

/// The shipped bundle's theme, embedded beside its values for the same reason its codec grid is: a
/// build with no bundle selected at runtime reads no file.
pub const SHIPPED_THEME: &str = include_str!("../bundles/chrome-152-linux-20261016T1022/theme.json");

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Scrollbar {
    thickness: f32,
    thin_thickness: f32,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct MenuList {
    inline_padding: f32,
    block_padding: f32,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThemeFile {
    scrollbar: Scrollbar,
    menu_list: MenuList,
}

/// Parse a `theme.json`.
pub fn derive(json: &str) -> Result<ThemeProfile, serde_json::Error> {
    let t: ThemeFile = serde_json::from_str(json)?;
    Ok(ThemeProfile {
        scrollbar_thickness: t.scrollbar.thickness,
        scrollbar_thin_thickness: t.scrollbar.thin_thickness,
        menu_list_inline_padding: t.menu_list.inline_padding,
        menu_list_block_padding: t.menu_list.block_padding,
        font_map: 0,
        min_logical_font_size: 0.0,
        submit_label: "",
        reset_label: "",
    })
}

fn cache() -> &'static Mutex<BTreeMap<String, Option<ThemeProfile>>> {
    static CACHE: OnceLock<Mutex<BTreeMap<String, Option<ThemeProfile>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// The theme of the bundle in `dir`, read at most once per process.
///
/// `dir` is the bundle's directory when the persona was loaded from disk, `None` for the embedded one.
/// KEYED BY THE DIRECTORY, not by the capture id, because the id costs a manifest parse and layout
/// asks for the theme on every geometry read: `layout_metrics::measuring_every_element_is_linear_
/// and_not_quadratic` measured 6,000 reads at 5.8 s of CPU when each one parsed the TOML first.
pub fn for_bundle(dir: Option<&std::path::Path>) -> Option<ThemeProfile> {
    let key = dir.map(|d| d.display().to_string()).unwrap_or_default();
    if let Ok(c) = cache().lock() {
        if let Some(hit) = c.get(&key) {
            return *hit;
        }
    }
    let id = dir.and_then(|d| d.file_name()).map(|n| n.to_string_lossy().to_string());
    // The worn bundle's own font names; the embedded theme is the shipped bundle's, and
    // `0` already picks that map.
    let built = load(id.as_deref().unwrap_or("<embedded>"), dir)
        .map(|t| ThemeProfile { font_map: id.as_deref().map_or(0, spec::font_map_key), ..t });
    if let Ok(mut c) = cache().lock() {
        c.insert(key, built);
    }
    built
}

fn load(capture_id: &str, dir: Option<&std::path::Path>) -> Option<ThemeProfile> {
    let (json, source): (std::borrow::Cow<'static, str>, String) = match dir {
        None => (SHIPPED_THEME.into(), "<embedded theme>".to_string()),
        Some(dir) => {
            let path = crate::surface::artifact_in(dir, "theme")?;
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
        Ok(t) => Some(t),
        Err(e) => {
            eprintln!("[korvus] persona {capture_id}: {source} does not parse as a theme: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundles() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("bundles")
    }

    /// Every bundle carries a theme, it parses, and the embedded copy is the shipped bundle's file.
    /// The numbers themselves are NOT asserted here: they are the capture's, and a test that spelled
    /// them would be a second, hand-written copy of the bundle.
    #[test]
    fn every_bundle_has_a_theme_and_the_embedded_one_is_the_shipped_file() {
        let mut seen = 0;
        for entry in std::fs::read_dir(bundles()).unwrap() {
            let dir = entry.unwrap().path();
            if !dir.join("manifest.toml").exists() {
                continue;
            }
            let id = dir.file_name().unwrap().to_string_lossy().to_string();
            let path = crate::surface::artifact_in(&dir, "theme")
                .unwrap_or_else(|| panic!("{id}: the manifest names no `theme` artifact"));
            let text = std::fs::read_to_string(&path).unwrap();
            let t = derive(&text).unwrap_or_else(|e| panic!("{id}: theme.json does not parse: {e}"));
            assert!(t.scrollbar_thin_thickness <= t.scrollbar_thickness, "{id}: thin is not thinner");
            if id == crate::bundle::Bundle::parse(crate::BUNDLE_MANIFEST).unwrap().capture.id {
                assert_eq!(text, SHIPPED_THEME, "the embedded theme is not the shipped bundle's file");
            }
            seen += 1;
        }
        assert!(seen >= 1, "no bundles found");
    }
}
