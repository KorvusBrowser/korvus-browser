//! The bundle's recorded SURFACE, reduced to a selector the installer can join against.
//!
//! # What this is for
//!
//! A bundle swap used to change what the browser SAYS and not what it HAS. `values.json` switched at
//! runtime — user agent, brands, screen, GPU — while which interfaces existed, which members each
//! prototype owned and what order `Object.getOwnPropertyNames` reported them in were all compiled in
//! from ONE capture. So the engine wearing the Chrome bundle answered brands `Not;A=Brand`/Chromium
//! 150 next to `navigator.brave.isBrave() === true`: an incoherent device, which is exactly what
//! `docs/PRINCIPLES.md` exists to forbid, and what
//! a persona diff measured as four leaks.
//!
//! This module closes it from the persona's side. The bundle already records both realms' surfaces
//! (`surface.json`, `worker.surface.json`, the same files `tools/sync/gen_shells.py` generates the
//! registry from); [`for_bundle`] parses each into a [`spec::RealmSurface`] — the global's own-key
//! order and every interface's member order — and `install::installer` mounts only what it names, in
//! the order it gives.
//!
//! # Why the shipped bundle's two surfaces are EMBEDDED and the rest are read from disk
//!
//! [`crate::SHIPPED_VALUES`] and [`crate::BUNDLE_MANIFEST`] are `include_str!` so that a build with
//! nothing selected has NO RUNTIME FILE DEPENDENCY: the identity is deterministic and the tests are
//! hermetic. Making the surface a disk read for the default persona would have quietly traded that
//! away — a deployed binary run outside its build checkout would have found no surface and installed
//! nothing. So the shipped bundle's surfaces are embedded on exactly the same terms as its values,
//! and a bundle named at runtime reads its own two files out of its own directory, on exactly the
//! same terms as its `values.json`. Both halves therefore behave the way the bundle they belong to
//! already behaved, and I6 (the bundle is one directory) is untouched.
//!
//! # Cost, stated
//!
//! `surface.json` is 2.4 MB and `worker.surface.json` 0.9 MB. They are parsed ONCE PER PROCESS per
//! (bundle, realm) — [`CACHE`] — not once per context, and the parse keeps only names: everything
//! else in those files (descriptors, nativeness, function names and lengths) is the GRADER's input
//! and is read from the bundle by the tests that grade, never by the engine.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use spec::{MemberOrder, RealmSurface, SurfaceRealm};

/// The shipped bundle's window surface, embedded beside its values for the reason in the module doc.
pub const SHIPPED_SURFACE: &str = include_str!("../bundles/chrome-152-linux-20261016T1022/surface.json");
/// The shipped bundle's worker surface, likewise.
pub const SHIPPED_WORKER_SURFACE: &str =
    include_str!("../bundles/chrome-152-linux-20261016T1022/worker.surface.json");

/// Only the two fields the installer asks for. serde skips every other key without allocating, which
/// is what keeps a 2.4 MB parse to names.
#[derive(serde::Deserialize)]
struct RawSurface {
    #[serde(default)]
    globals: Vec<String>,
    #[serde(default)]
    interfaces: HashMap<String, RawIface>,
}

#[derive(serde::Deserialize)]
struct RawIface {
    #[serde(default)]
    members: Vec<RawMember>,
    #[serde(default)]
    statics: Vec<RawMember>,
    /// `Object.getOwnPropertyNames(X.prototype).indexOf("constructor")`. It is NOT a
    /// member row: the capture rig filters `constructor` out of `members` because its descriptor is
    /// identical on every interface of every browser, and the only thing it carries is its POSITION.
    /// Recorded on BOTH artifacts of all four bundles since FP18 re-read the worker realm from the
    /// browsers (it was a window-only field until then, bridged by a cross-realm borrow under R22 —
    /// deleted, because the two realms' captures were then measured to agree on all 1,095 interfaces
    /// the borrow covered). A bundle whose artifact predates that leaves this `None`, and the
    /// installer keeps the placement it had rather than inventing a number.
    #[serde(rename = "ctorIndex")]
    ctor_index: Option<usize>,
}

#[derive(serde::Deserialize)]
struct RawMember {
    name: String,
}

fn parse(capture_id: &str, json: &str) -> Result<RealmSurface, String> {
    let raw: RawSurface = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let interfaces = raw
        .interfaces
        .into_iter()
        .map(|(name, i)| {
            (
                name,
                MemberOrder {
                    members: i.members.into_iter().map(|m| m.name).collect(),
                    statics: i.statics.into_iter().map(|m| m.name).collect(),
                    ctor_index: i.ctor_index,
                },
            )
        })
        .collect();
    Ok(RealmSurface { capture_id: capture_id.to_string(), globals: raw.globals, interfaces })
}

type Cache = Mutex<HashMap<(String, SurfaceRealm), Option<Arc<RealmSurface>>>>;

fn cache() -> &'static Cache {
    static C: OnceLock<Cache> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The surface of the bundle with this capture id, for one realm — parsed at most once per process.
///
/// `dir` is the bundle's directory when the persona was loaded from disk, `None` for the embedded
/// one. A bundle directory this binary has never seen is read here exactly like any other; what it
/// cannot supply is a ROW for a member no table carries, which the installer reports as a gap.
///
/// A surface file that is missing or malformed is `None`, not a panic: the persona still describes a
/// device, and the installer falls back to the tables' own rows — the same answer a bare runtime
/// gets. It is reported once on stderr rather than swallowed, because a bundle whose surface does not
/// parse is a bundle that has stopped describing a browser.
pub fn for_bundle(capture_id: &str, dir: Option<&std::path::Path>, realm: SurfaceRealm) -> Option<Arc<RealmSurface>> {
    let key = (capture_id.to_string(), realm);
    if let Ok(c) = cache().lock() {
        if let Some(hit) = c.get(&key) {
            return hit.clone();
        }
    }
    let built = load(capture_id, dir, realm);
    if let Ok(mut c) = cache().lock() {
        c.insert(key, built.clone());
    }
    built
}

/// Seed [`for_bundle`]'s cache with a surface parsed elsewhere -- a snapshot of a page realm that wore this bundle:
/// this process answers it from then on and never parses the bundle's JSON for it. A surface already cached
/// stays.
pub fn seed(capture_id: &str, realm: SurfaceRealm, surface: Arc<RealmSurface>) {
    if let Ok(mut c) = cache().lock() {
        c.entry((capture_id.to_string(), realm)).or_insert(Some(surface));
    }
}

fn load(capture_id: &str, dir: Option<&std::path::Path>, realm: SurfaceRealm) -> Option<Arc<RealmSurface>> {
    let (json, source): (std::borrow::Cow<'static, str>, String) = match dir {
        None => (
            match realm {
                SurfaceRealm::Window => SHIPPED_SURFACE.into(),
                SurfaceRealm::Worker => SHIPPED_WORKER_SURFACE.into(),
            },
            format!("<embedded {}>", realm.artifact_role()),
        ),
        Some(dir) => {
            let path = artifact_in(dir, realm.artifact_role())?;
            match std::fs::read_to_string(&path) {
                Ok(s) => (s.into(), path.display().to_string()),
                Err(e) => {
                    eprintln!("[korvus] persona {capture_id}: cannot read {}: {e}", path.display());
                    return None;
                }
            }
        }
    };
    if std::env::var_os("KORVUS_SNAPSHOT_REPORT").is_some() {
        eprintln!("korvus: persona {capture_id}: the {} surface parsed from JSON ({source})", realm.artifact_role());
    }
    match parse(capture_id, &json) {
        Ok(s) => Some(Arc::new(s)),
        Err(e) => {
            eprintln!("[korvus] persona {capture_id}: {source} does not parse as a surface: {e}");
            None
        }
    }
}

/// The path a bundle's own manifest gives for one artifact role. The manifest is the index of the
/// bundle directory (I6), so this reader spells no file name of its own.
///
/// Shared with [`crate::codecs`], which resolves its bundle artifact the same way: one reader of the
/// manifest index, so no second place learns what a bundle's files are called.
pub(crate) fn artifact_in(dir: &std::path::Path, role: &str) -> Option<std::path::PathBuf> {
    let manifest = std::fs::read_to_string(dir.join("manifest.toml")).ok()?;
    let b = crate::Bundle::parse(&manifest).ok()?;
    let a = b.artifacts.iter().find(|a| a.role == role)?;
    Some(dir.join(&a.path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_shipped_surfaces_parse_and_carry_both_realms() {
        let w = for_bundle("chrome-152-linux-20261016T1022", None, SurfaceRealm::Window)
            .expect("the embedded window surface parses");
        let k = for_bundle("chrome-152-linux-20261016T1022", None, SurfaceRealm::Worker)
            .expect("the embedded worker surface parses");
        // The two numbers a persona diff measured for this bundle.
        assert_eq!(w.globals.len(), 1230, "the window capture's own-global count");
        assert_eq!(k.globals.len(), 335, "the worker capture's own-global count");
        // The alias is the EXPOSED name in each realm, which is what the installer looks up.
        assert_eq!(w.members_of("Navigator").map(|m| m.members.len()), Some(80));
        assert_eq!(k.members_of("WorkerNavigator").map(|m| m.members.len()), Some(22));
        assert!(!w.members_of("Navigator").unwrap().members.iter().any(|m| m == "brave"),
            "Chrome's capture records no navigator.brave");
        assert!(w.has_global("HTMLBodyElement"));
        assert!(!k.has_global("HTMLBodyElement"), "a worker has no HTML element interfaces");
    }

    #[test]
    fn a_missing_bundle_directory_is_none_not_a_panic() {
        assert!(for_bundle("no-such-bundle", Some(std::path::Path::new("/nonexistent")), SurfaceRealm::Window).is_none());
    }
}
