//! `persona` — concrete device profiles (data). A persona is ONE coherent identity: its navigator,
//! screen, UA Client-Hints, plugins, connection, and transport fingerprint all describe the same real
//! device (captured from a real browser, per "the browser is the specification"). A "Frankenstein"
//! mix (e.g. a Mac UA with an Android platform) is an instant bot tell, so all layers read from one
//! profile.
//!
//! - [`CapturedPersona`]: a real captured profile (embedded JSON), the identity Korvus wears in
//!   practice. GS.3 wired the engine's native navigator/screen getters + `op_persona_hints` (since removed) to it.

use serde::Deserialize;
use spec::{
    Brand, ClientHints, Connection, NavigatorExtra, NavigatorProfile, Persona, PluginInfo,
    MemoryProfile, ScreenProfile, TlsProfile,
};

/// The default captured profile: the shipped bundle (`bundles/chrome-152-linux-20261016T1022`, a real
/// Linux Chrome 150 captured on this host), harvested in one session so navigator, screen, UA-CH,
/// plugins, connection, timezone, WebGL, and fonts are mutually coherent. Embedded at build time (no
/// runtime file dependency) so the identity is deterministic and tests are hermetic; `KORVUS_BUNDLE`
/// selects another bundle directory at runtime.
pub mod bundle;
/// The shared persona format (shared/personas/FORMAT.md): read a document, wear its device.
pub mod document;
pub use bundle::Bundle;

/// The bundle's recorded SURFACE as a selector — which interfaces and members this device exposes in
/// each realm, and in what order. See [`surface`] for why the shipped bundle's two files are
/// embedded and a runtime-selected bundle's are read from its own directory.
pub mod surface;

/// The bundle's recorded CODEC GRID — what this device answers to "can you play this?", which is a
/// fact about the build it was captured from and not about this engine. See [`codecs`] for the 274
/// observables that were Brave's until it existed, and for why it is a claim and not a capability.
pub mod codecs;

/// The bundle's recorded PLATFORM THEME — a classic scrollbar's thickness and a drop-down's theme
/// paddings, read back off layout by the capture rig. See [`theme`].
pub mod theme;
pub mod computed_style;
/// The names a `CSSStyleDeclaration` lists as its named properties on the worn device. See
/// [`css_named_properties`].
pub mod css_named_properties;
/// The device's own SDP offer -- its receive-only audio and video sections. See [`webrtc_offer`].
pub mod webrtc_offer;
/// The device's `RTCRtpReceiver`/`RTCRtpSender.getCapabilities` answers. See [`webrtc_capabilities`].
pub mod webrtc_capabilities;

/// The objects this device reaches through `navigator` without exposing their interface on the
/// global — `navigator.brave`. Their NAMES are browser-identifying literals, so they
/// live here rather than in the engine; see [`hidden`] for the whole argument.
pub mod hidden;

/// The values this build serves, embedded.
///
/// PUBLIC ON PURPOSE. The grader and its harness used to reach for the profile by PATH --
/// `the persona crate's embedded values (`persona::SHIPPED_VALUES`)`, written out in three files --
/// so switching the shipped bundle left them grading the engine against a persona it no longer wore,
/// silently and with no error. A path is a guess about where the truth lives; this is the truth.
pub const SHIPPED_VALUES: &str = include_str!("../bundles/chrome-152-linux-20261016T1022/values.json");

/// The manifest of the bundle this build ships, embedded beside its values so the two cannot drift
/// apart in a deployment. See [`bundle`] for what a bundle is and why the persona is one.
pub const BUNDLE_MANIFEST: &str = include_str!("../bundles/chrome-152-linux-20261016T1022/manifest.toml");

/// The bundle this build ships, parsed. Panics only if the embedded manifest is malformed, which a
/// unit test catches long before a run does.
pub fn shipped_bundle() -> Bundle {
    Bundle::parse(BUNDLE_MANIFEST).expect("the embedded bundle manifest must parse")
}

/// The Chrome release this build of korvus is for: `tools/version/chrome.toml`'s `chrome`, compiled in. A persona
/// plugin must come from the same milestone ([`CapturedPersona::from_dir`]).
pub fn build_chrome() -> &'static str {
    static V: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    V.get_or_init(|| {
        include_str!("../../../tools/version/chrome.toml")
            .lines()
            .find_map(|l| l.strip_prefix("chrome = \"").and_then(|r| r.strip_suffix('"')))
            .expect("tools/version/chrome.toml names its `chrome` release")
            .to_string()
    })
}

fn milestone(version: &str) -> &str {
    version.split('.').next().unwrap_or("")
}

/// The Chromium release a persona's values were captured from: its `Chromium` brand's full version (Chrome's own
/// version for Chrome), else `uaFullVersion`.
fn chromium_version(values: &serde_json::Value) -> Option<String> {
    let he = values.pointer("/nav/userAgentData/highEntropy")?;
    he.get("fullVersionList")
        .and_then(|l| l.as_array())
        .and_then(|l| l.iter().find(|b| b.get("brand").and_then(|s| s.as_str()) == Some("Chromium")))
        .and_then(|b| b.get("version"))
        .or_else(|| he.get("uaFullVersion"))
        .and_then(|s| s.as_str())
        .map(str::to_string)
}

/// The directory of the bundle this build ships (`plugins/korvus-persona/bundles/<id>`), on the
/// machine that built it. The values and the manifest are embedded; everything else the manifest
/// declares -- the surfaces, the oracle, the render reference, the codecs grid, the font metrics --
/// is read from here by the tests and tools that need it. See [`shipped_artifact`].
pub fn shipped_bundle_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("bundles").join(shipped_bundle().capture.id)
}

/// The on-disk path of one of the shipped bundle's artifacts, by manifest role (`surface`,
/// `worker-surface`, `oracle`, `render-reference`, `codecs`, `font-metrics`, `values`,
/// `ua-stylesheet`). The manifest is the index of the bundle directory, so no reader spells
/// a file name of its own. Panics on a role the manifest does not carry: that is a request for a file
/// that cannot exist.
pub fn shipped_artifact(role: &str) -> std::path::PathBuf {
    let b = shipped_bundle();
    let a = b
        .artifacts
        .iter()
        .find(|a| a.role == role)
        .unwrap_or_else(|| panic!("the shipped bundle manifest has no artifact with role {role:?}"));
    shipped_bundle_dir().join(&a.path)
}

/// A persona backed by a real captured profile. Parses the embedded JSON once at construction and
/// maps it into the `spec` device types the engine reads. This is the source of truth for every
/// persona-visible VALUE (per the operating rule: no hardcoded values outside the persona/profiles).
#[derive(Debug, Clone)]
pub struct CapturedPersona {
    raw: RawProfile,
    /// The manifest of a bundle loaded from disk; `None` for the embedded one, which uses
    /// [`BUNDLE_MANIFEST`]. Carried so a runtime-selected bundle declares its OWN known
    /// incoherences rather than the embedded bundle's, which would be a different device's excuses.
    manifest: Option<String>,
    /// The DIRECTORY a disk-loaded bundle came from; `None` for the embedded one. Carried for
    /// exactly the reason `manifest` is: [`Persona::surface`] must read the surface of the bundle
    /// this persona was LOADED from. Reading the embedded one for a disk-loaded persona would hand
    /// it another device's interface list — the incoherence this whole door exists to prevent, one
    /// layer down.
    dir: Option<std::path::PathBuf>,
    /// This bundle's `farbling.json`, the embedded one for the shipped persona and the
    /// disk one for a `KORVUS_BUNDLE` selection. `None` when the bundle ships no farbling artifact,
    /// which is how a non-farbling browser (Chrome/Edge/Opera) serves its scalars static -- the same
    /// reason `dir`/`manifest` are per-bundle: a persona's farbling must be its OWN device's, not the
    /// embedded Brave's. Parsed lazily by [`Persona::farbling`].
    farbling_json: Option<String>,
}

impl CapturedPersona {
    /// A bundle of this checkout by capture id (`$KORVUS_BUNDLE`, [`plugged_in`](Self::plugged_in)'s second door); `None`
    /// or empty is the embedded bundle, which only tests and generators ask for.
    ///
    /// Split out so the selection rules can be tested without mutating the process environment --
    /// `set_var` is global, and a test that sets it races every other test in the binary.
    pub fn select(id: Option<&str>) -> Result<Self, String> {
        let Some(id) = id else { return Ok(Self::shipped()) };
        if id.is_empty() {
            return Ok(Self::shipped());
        }
        if id.contains('/') || id.contains("..") {
            return Err(format!("KORVUS_BUNDLE={id:?} is a capture id, not a path"));
        }
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("bundles").join(id);
        Self::from_dir(&dir).map_err(|e| format!("KORVUS_BUNDLE={id:?}: {e}"))
    }

    /// A persona plugin: a persona folder -- a captured bundle directory, `values.json`, `manifest.toml` and the
    /// artifacts the manifest names -- read at run time. Refused when it was captured from another Chrome milestone
    /// than this build is for ([`build_chrome`]): korvus's V8 is that milestone's, and a persona claiming another
    /// would contradict the engine under it. A patch release of the same milestone is accepted.
    pub fn from_dir(dir: &std::path::Path) -> Result<Self, String> {
        let values = dir.join("values.json");
        let json = std::fs::read_to_string(&values)
            .map_err(|e| format!("persona {}: cannot read {}: {e}", dir.display(), values.display()))?;
        let v: serde_json::Value = serde_json::from_str(&json).map_err(|e| format!("persona {}: {e}", dir.display()))?;
        let chromium = chromium_version(&v).ok_or_else(|| format!("persona {}: its values name no Chromium version", dir.display()))?;
        if milestone(&chromium) != milestone(build_chrome()) {
            return Err(format!(
                "persona {} was captured from Chromium {chromium}; this korvus is built for Chrome {} (its V8 is that \
                 milestone's) -- plug in a persona captured from Chrome {}",
                dir.display(),
                build_chrome(),
                milestone(build_chrome())
            ));
        }
        let mut me = Self::from_json(&json).map_err(|e| format!("persona {}: {e}", dir.display()))?;
        me.manifest = std::fs::read_to_string(dir.join("manifest.toml")).ok();
        me.farbling_json = std::fs::read_to_string(dir.join("farbling.json")).ok();
        me.dir = Some(dir.to_path_buf());
        Ok(me)
    }

    /// The build this process wears for `os` (`windows`, `macos`, `linux`, `android`: the persona format's names) --
    /// korvus does not start without one -- from the first of:
    ///   1. `$KORVUS_PERSONA_DIR`, a build folder (what `korvus --persona <folder>` sets), whatever its OS;
    ///   2. `$KORVUS_BUNDLE`, a capture id under this checkout's `plugins/korvus-persona/bundles` (the tools here);
    ///   3. `personas/` beside the executable, how a release ships its builds: the folder for `os` captured from this
    ///      build's milestone;
    ///   4. a development build run from a checkout's `target/` directory: that checkout's own bundles, the same way.
    ///
    /// None of them is an error naming where it looked. A binary never wears the embedded bundle: that is the
    /// generators' and the tests' input, not a fallback. Which OS a process wears is its identity's: the host's for
    /// korvus's own (`from_env`), the document's for an identity a plugin answered.
    pub fn plugged_in(os: &str) -> Result<Self, String> {
        let exe = std::env::current_exe().ok();
        Self::resolve_plugin(
            std::env::var_os("KORVUS_PERSONA_DIR").map(std::path::PathBuf::from),
            std::env::var("KORVUS_BUNDLE").ok(),
            exe.as_deref(),
            os,
        )
    }

    /// [`plugged_in`](Self::plugged_in) with its inputs passed in, so the rules are testable without the process
    /// environment (`set_var` races every other test in the binary).
    pub fn resolve_plugin(
        dir: Option<std::path::PathBuf>,
        bundle: Option<String>,
        exe: Option<&std::path::Path>,
        os: &str,
    ) -> Result<Self, String> {
        if let Some(d) = dir.filter(|d| !d.as_os_str().is_empty()) {
            return Self::from_dir(&d);
        }
        if let Some(id) = bundle.filter(|s| !s.is_empty()) {
            return Self::select(Some(&id));
        }
        let mut looked = Vec::new();
        if let Some(exe_dir) = exe.and_then(|e| e.parent()) {
            let beside = exe_dir.join("personas");
            looked.push(beside.display().to_string());
            if let Some(p) = Self::pick_in(&beside, os)? {
                return Ok(p);
            }
            let checkout = exe_dir.ancestors().find(|a| a.file_name().is_some_and(|n| n == "target")).and_then(|t| t.parent());
            if let Some(bundles) = checkout.map(|c| c.join("plugins/korvus-persona/bundles")).filter(|b| b.is_dir()) {
                looked.push(bundles.display().to_string());
                if let Some(p) = Self::pick_in(&bundles, os)? {
                    return Ok(p);
                }
            }
        }
        Err(format!(
            "no persona plugged in, and korvus does not start without one: plug one in with `korvus --persona <folder>`, \
             `KORVUS_PERSONA_DIR=<folder>`, or a `personas/` folder beside the executable (looked in: {})",
            if looked.is_empty() { "nowhere: the executable's path is unknown".to_string() } else { looked.join(", ") }
        ))
    }

    /// The build for `os` in `dir`: of the folders captured from this build's milestone, the first (by capture id)
    /// whose user agent names `os`. `Ok(None)` when `dir` holds no build folder at all (the caller tries the next
    /// door); an error when it holds folders but none for `os`, or only other milestones'.
    fn pick_in(dir: &std::path::Path, os: &str) -> Result<Option<Self>, String> {
        let want = ua_family(os);
        let Ok(rd) = std::fs::read_dir(dir) else { return Ok(None) };
        let mut folders: Vec<std::path::PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.join("values.json").is_file()).collect();
        folders.sort();
        if folders.is_empty() {
            return Ok(None);
        }
        let (mut fit, mut other) = (Vec::new(), Vec::new());
        for f in folders {
            match Self::from_dir(&f) {
                Ok(p) => fit.push(p),
                Err(e) => other.push(e),
            }
        }
        if fit.is_empty() {
            return Err(other.join("; "));
        }
        let fit_n = fit.len();
        let mut of_os: Vec<Self> = fit.into_iter().filter(|p| spec::coherence::os_family_from_ua(&Persona::navigator(p).user_agent) == Some(want)).collect();
        if of_os.is_empty() {
            return Err(format!("no {os} build in {}: none of the {fit_n} build(s) there is one", dir.display()));
        }
        of_os.sort_by(|a, b| a.bundle().capture.id.cmp(&b.bundle().capture.id));
        Ok(Some(of_os.swap_remove(0)))
    }

    /// The build named `id` (a capture id, a persona document's `build.bundle`) beside the executable or in this
    /// checkout's bundles.
    fn named(id: &str) -> Result<Self, String> {
        if id.contains('/') || id.contains("..") {
            return Err(format!("build {id:?} is a capture id, not a path"));
        }
        let exe = std::env::current_exe().ok();
        let exe_dir = exe.as_deref().and_then(|e| e.parent());
        let mut dirs: Vec<std::path::PathBuf> = exe_dir.map(|d| d.join("personas")).into_iter().collect();
        if let Some(c) = exe_dir.and_then(|d| d.ancestors().find(|a| a.file_name().is_some_and(|n| n == "target"))).and_then(|t| t.parent()) {
            dirs.push(c.join("plugins/korvus-persona/bundles"));
        }
        dirs.push(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("bundles"));
        for d in &dirs {
            if d.join(id).join("values.json").is_file() {
                return Self::from_dir(&d.join(id));
            }
        }
        Err(format!("no build {id:?} (looked in: {})", dirs.iter().map(|d| d.display().to_string()).collect::<Vec<_>>().join(", ")))
    }

    /// The build a persona document names: an explicit build first (`--persona`, `$KORVUS_PERSONA_DIR`,
    /// `$KORVUS_BUNDLE`, as for any run), then its `build.bundle`, then the build for its `build.os` or device's OS.
    pub fn for_document(doc: &document::Document) -> Result<Self, String> {
        let explicit = std::env::var_os("KORVUS_PERSONA_DIR").is_some_and(|d| !d.is_empty())
            || std::env::var("KORVUS_BUNDLE").is_ok_and(|b| !b.is_empty());
        if !explicit {
            if let Some(id) = doc.build.as_ref().and_then(|b| b.get("bundle")).and_then(serde_json::Value::as_str) {
                return Self::named(id);
            }
        }
        let os = doc.build.as_ref().and_then(|b| b.get("os")).and_then(serde_json::Value::as_str).unwrap_or(doc.os());
        Self::plugged_in(os)
    }

    /// Load the values of the shipped bundle (`bundles/chrome-152-linux-20261016T1022/values.json`).
    pub fn shipped() -> Self {
        Self::from_json(SHIPPED_VALUES).expect("the embedded bundle values must parse")
    }

    /// This persona's OWN bundle: the one it was loaded from, not whichever is embedded.
    ///
    /// The difference matters the moment `KORVUS_BUNDLE` is used. Reading the embedded manifest for
    /// a disk-loaded persona would hand it another device's transport preset and another device's
    /// list of excused incoherences -- and the audit would then pass for reasons that have nothing
    /// to do with the device actually being worn.
    pub fn bundle(&self) -> Bundle {
        Bundle::parse(self.manifest.as_deref().unwrap_or(crate::BUNDLE_MANIFEST))
            .expect("the bundle manifest must parse")
    }

    /// Parse a captured-profile JSON string into a persona.
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        Ok(Self { raw: serde_json::from_str(s)?, manifest: None, dir: None, farbling_json: None })
    }

    /// The directory this persona's bundle was loaded from — `None` for the embedded one, whose
    /// artifacts are compiled in. Used by [`Persona::surface`] and by the per-bundle gates, which
    /// must grade the engine against the bundle it is WEARING and not the one it embeds.
    pub fn bundle_dir(&self) -> Option<&std::path::Path> {
        self.dir.as_deref()
    }

    /// The on-disk path of one of the ACTIVE bundle's artifacts, by manifest role — the
    /// runtime-selected counterpart of [`crate::shipped_artifact`].
    ///
    /// The gates that compare the live surface to "the bundle" used to reach for
    /// [`crate::shipped_artifact`], which is the EMBEDDED bundle's: under `KORVUS_BUNDLE` they graded
    /// the engine wearing one device against the recording of another and called the difference a
    /// regression. This resolves through the bundle the persona actually came from.
    pub fn artifact(&self, role: &str) -> std::path::PathBuf {
        match self.bundle_dir() {
            None => crate::shipped_artifact(role),
            Some(dir) => {
                let b = self.bundle();
                let a = b
                    .artifacts
                    .iter()
                    .find(|a| a.role == role)
                    .unwrap_or_else(|| panic!("the active bundle manifest has no artifact with role {role:?}"));
                dir.join(&a.path)
            }
        }
    }
}

impl Default for CapturedPersona {
    fn default() -> Self {
        Self::shipped()
    }
}

impl CapturedPersona {
    /// Parse `farbling.json`'s scalar policy into a [`spec::FarblingPolicy`]. Reads only
    /// the scalars whose captured `farbled` flag is `true`: `navigator.hardwareConcurrency` (uniform
    /// `[2, N]`, N the single `unfarbled` key) and `navigator.deviceMemory` (uniform over `counts`'
    /// keys). Values come from the file, so the engine carries no browser constant (I4). `None` when
    /// the bundle ships no farbling artifact or it cannot be read.
    fn parse_farbling(&self) -> Option<spec::FarblingPolicy> {
        let raw = self.farbling_json.as_deref()?;
        let doc: serde_json::Value = serde_json::from_str(raw).ok()?;
        let scalars = doc.get("signals")?.get("scalars")?;
        let mut pol = spec::FarblingPolicy::default();
        if let Some(hwc) = scalars.get("navigator.hardwareConcurrency") {
            if hwc.get("farbled").and_then(|v| v.as_bool()) == Some(true) {
                // N = the unfarbled core count (the single key of `unfarbled`), which the policy says
                // to read HERE rather than from meta.machineCores.
                if let Some(n) = hwc.get("unfarbled").and_then(|u| u.as_object())
                    .and_then(|m| m.keys().next()).and_then(|k| k.parse::<u32>().ok())
                {
                    pol.hardware_concurrency_max = Some(n);
                }
            }
        }
        if let Some(dm) = scalars.get("navigator.deviceMemory") {
            if dm.get("farbled").and_then(|v| v.as_bool()) == Some(true) {
                if let Some(counts) = dm.get("counts").and_then(|c| c.as_object()) {
                    let mut set: Vec<f64> = counts.keys().filter_map(|k| k.parse::<f64>().ok()).collect();
                    set.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    if !set.is_empty() {
                        pol.device_memory_set = set;
                    }
                }
            }
        }
        if pol.hardware_concurrency_max.is_none() && pol.device_memory_set.is_empty() {
            return None;
        }
        Some(pol)
    }
}

/// The context snapshot (slice 3): [`Persona::source_stamp`] for a capture read from `dir` -- every file under
/// it, by path, size and modification time. The shipped persona has no directory: 0.
fn stamp_dir(dir: Option<&std::path::Path>) -> u64 {
    use std::hash::{Hash, Hasher};
    let Some(dir) = dir else { return 0 };
    let mut rows = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let path = e.path();
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() {
                stack.push(path);
            } else {
                let mtime = md.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos()).unwrap_or(0);
                rows.push((path.strip_prefix(dir).map(|p| p.to_path_buf()).unwrap_or(path), md.len(), mtime));
            }
        }
    }
    rows.sort();
    // SipHash with fixed keys: the same files give the same stamp in every process
    let mut h = std::collections::hash_map::DefaultHasher::new();
    dir.hash(&mut h);
    rows.hash(&mut h);
    h.finish() | 1
}

impl Persona for CapturedPersona {
    fn source_stamp(&self) -> u64 {
        stamp_dir(self.dir.as_deref())
    }
    fn seed_surface(&self, realm: spec::SurfaceRealm, surface: std::sync::Arc<spec::RealmSurface>) {
        // into the cache `surface` reads, under this persona's OWN bundle -- as `surface` keys it
        crate::surface::seed(&self.bundle().capture.id, realm, surface)
    }
    fn farbling(&self) -> Option<spec::FarblingPolicy> {
        self.parse_farbling()
    }

    fn navigator(&self) -> NavigatorProfile {
        let n = &self.raw.nav;
        NavigatorProfile {
            user_agent: n.user_agent.clone(),
            platform: n.platform.clone(),
            hardware_concurrency: n.hardware_concurrency,
            device_memory: n.device_memory,
            languages: n.languages.clone(),
            accept_language: n.accept_language.clone(),
            global_privacy_control: n.global_privacy_control,
            accept_navigation: n.accept_navigation.clone(),
            third_party_cookies_blocked: n.third_party_cookies.as_deref() == Some("blocked"),
            keyboard_layout: n.keyboard_layout_map.clone(),
        }
    }

    fn screen(&self) -> ScreenProfile {
        let s = &self.raw.screen;
        ScreenProfile {
            width: s.width,
            height: s.height,
            color_depth: s.color_depth,
            device_pixel_ratio: self.raw.win.device_pixel_ratio,
            avail_width: s.avail_width,
            avail_height: s.avail_height,
            avail_top: s.avail_top,
            avail_left: s.avail_left,
            window_inner_width: self.raw.win.inner_width,
            window_inner_height: self.raw.win.inner_height,
            window_outer_width: self.raw.win.outer_width,
            window_outer_height: self.raw.win.outer_height,
            window_screen_x: self.raw.win.screen_x,
            window_screen_y: self.raw.win.screen_y,
            // The capture's `screen.orientation`, whole or not at all: a bundle that recorded no
            // orientation leaves the type empty and the engine exposes no orientation object, rather
            // than pairing a captured angle with an invented type (see `spec::ScreenProfile`).
            orientation_type: s.orientation.ty.clone(),
            orientation_angle: s.orientation.angle,
            refresh_rate: s.refresh_rate,
        }
    }

    fn memory(&self) -> MemoryProfile {
        let m = self.raw.memory.clone().unwrap_or_default();
        MemoryProfile {
            js_heap_size_limit: m.js_heap_size_limit,
            total_js_heap_size: m.total_js_heap_size,
            used_js_heap_size: m.used_js_heap_size,
            empty_document: m.empty_document.map(|e| (e.total_js_heap_size, e.used_js_heap_size)),
        }
    }

    fn transport_profile(&self) -> TlsProfile {
        // THE BUNDLE DECLARES THIS. It used to be DERIVED from the UA's Chrome major -- which made
        // the coherence audit's "UA major vs TLS profile id" check compare the UA against a number
        // computed from the UA. A tautology that could never fail, sitting in the audit as though it
        // were evidence, while the bytes on the wire came from a `wreq_util::Emulation::Chrome137`
        // constant in the transport crate that nothing compared to anything.
        //
        // Reading it from the manifest makes the check real, and the first thing it reports is true:
        // this bundle claims Chrome 151 and emits Chrome 137, because the pinned wreq-util has no
        // newer preset. That is recorded as `known_incoherence "tls-version"` rather than hidden by
        // an identity.
        let b = self.bundle();
        let b = Bundle::parse(self.manifest.as_deref().unwrap_or(crate::BUNDLE_MANIFEST))
            .unwrap_or(b);
        // The HEADERS-frame priority is the bundle's too, and for the same reason: it is one field of
        // the wire that this capture's browser was measured putting on a socket, and the preset it
        // corrects is the preset named on the line above. A bundle that declares none keeps the
        // preset's, which is what every bundle did before anyone read one off a socket.
        let h2_priority = b.transport.headers_priority.map(|p| spec::H2Priority {
            dep_stream: p.stream,
            weight: p.weight,
            exclusive: p.exclusive,
        });
        // HTTP/3 the same way: the preset the capture's browser was measured speaking, or none.
        // ...and the ClientHello's `trust_anchors` extension, measured the same way (Chrome 152).
        TlsProfile {
            id: b.transport.emulation,
            platform: b.capture.os,
            h2_priority,
            quic: b.transport.quic,
            trust_anchors: b.transport.trust_anchors,
        }
    }

    fn known_incoherences(&self) -> Vec<String> {
        // Straight from the bundle's OWN manifest, where each id carries a written reason and a fix.
        // Nothing is hardcoded here on purpose: an exception that lives in Rust is an exception
        // nobody re-reads, and the whole point of the list is that it is read when it changes.
        // A runtime-selected bundle uses its own, never the embedded one's -- another device's
        // excuses are not this device's.
        let text = self.manifest.as_deref().unwrap_or(crate::BUNDLE_MANIFEST);
        Bundle::parse(text)
            .map(|b| b.known_incoherences.into_iter().map(|k| k.id).collect())
            .unwrap_or_default()
    }

    fn surface(&self, realm: spec::SurfaceRealm) -> Option<std::sync::Arc<spec::RealmSurface>> {
        // Through the persona's OWN bundle, for the same reason `known_incoherences` is: the surface
        // of the device being worn, never the embedded one's. Parsed at most once per process per
        // (bundle, realm) -- see `surface::for_bundle`.
        crate::surface::for_bundle(&self.bundle().capture.id, self.bundle_dir(), realm)
    }

    fn codecs(&self) -> Option<std::sync::Arc<spec::CodecGrid>> {
        // Through the persona's OWN bundle, exactly as `surface` is. An engine wearing Opera must
        // answer Opera's grid -- it has no proprietary H.264/AAC decoders -- and reading the
        // embedded one here would hand it Brave's, which is the leak this exists to close.
        crate::codecs::for_bundle(&self.bundle().capture.id, self.bundle_dir())
    }

    fn theme(&self) -> Option<spec::ThemeProfile> {
        // Through the persona's OWN bundle, as `codecs` is: the scrollbar a page gives up client
        // width for is the worn device's. Its minimum logical font size is its UI locale's (Chrome's
        // locale table), the locale being the one the capture's `navigator.language` reports -- read off the capture
        // itself: through `navigator()` every call cloned the whole profile (keyboard map and all) to read one
        // string, and layout asks for the theme on every geometry read -- 0.6 s of costco.com's 6.9 s critical
        // path (2026-10-22).
        let locale = self.raw.nav.languages.first().map(String::as_str).unwrap_or_default();
        crate::theme::for_bundle(self.bundle_dir())
            .map(|t| {
                let (submit_label, reset_label) = spec::form_button_labels_for(locale);
                spec::ThemeProfile { min_logical_font_size: spec::min_logical_font_size_for(locale), submit_label, reset_label, ..t }
            })
    }

    fn hidden_objects(&self) -> Vec<spec::HiddenObject> {
        // Gated on what THIS device's window capture records, never on which bundle is worn: a
        // capture with no `Navigator.brave` produces no Brave object, and the engine then has
        // nothing to mount even before the selector drops the row.
        let s = Persona::surface(self, spec::SurfaceRealm::Window);
        crate::hidden::descriptors_for(s.as_deref())
    }

    fn window_namespaces(&self) -> Vec<spec::WindowNamespace> {
        // Same gate, one holder over: the capture's own-globals list decides, so a device that does
        // not record `opr` produces none and the engine mounts nothing.
        let s = Persona::surface(self, spec::SurfaceRealm::Window);
        crate::hidden::window_namespaces_for(s.as_deref())
    }

    fn chrome_object_keys(&self) -> Vec<String> {
        // Straight off this device's own `values.json`, like every other captured scalar. Opera 135
        // records four keys where the other three record three, and the extra one is THIRD.
        self.raw.chrome.keys.clone()
    }

    fn navigator_extra(&self) -> NavigatorExtra {
        let n = &self.raw.nav;
        NavigatorExtra {
            vendor: n.vendor.clone(),
            vendor_sub: n.vendor_sub.clone(),
            app_name: n.app_name.clone(),
            app_code_name: n.app_code_name.clone(),
            product: n.product.clone(),
            product_sub: n.product_sub.clone(),
            max_touch_points: n.max_touch_points,
            cookie_enabled: n.cookie_enabled,
            pdf_viewer_enabled: n.pdf_viewer_enabled,
            do_not_track: n.do_not_track.clone(),
            cpu_performance: n.cpu_performance,
        }
    }

    fn client_hints(&self) -> ClientHints {
        let n = &self.raw.nav;
        let he = &n.user_agent_data.high_entropy;
        let map_brands = |bs: &[RawBrand]| -> Vec<Brand> {
            bs.iter().map(|b| Brand { brand: b.brand.clone(), version: b.version.clone() }).collect()
        };
        ClientHints {
            // Low-entropy brands come from userAgentData.brands (short versions); the high-entropy
            // fullVersionList carries the full "151.0.7922.76" strings.
            brands: map_brands(&n.user_agent_data.brands),
            full_version_list: map_brands(&he.full_version_list),
            mobile: n.user_agent_data.mobile,
            platform: n.user_agent_data.platform.clone(),
            architecture: he.architecture.clone(),
            bitness: he.bitness.clone(),
            model: he.model.clone(),
            platform_version: he.platform_version.clone(),
            ua_full_version: he.ua_full_version.clone(),
            wow64: he.wow64,
            form_factors: he.form_factors.clone(),
            permissions: n.permissions.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            storage_quota: n.storage_estimate.as_ref().map(|e| (e.quota, e.usage)),
            media_device_kinds: n.media_devices.kinds.clone(),
            supported_constraints: n.media_devices.supported.iter().filter(|(_, on)| **on).map(|(k, _)| k.clone()).collect(),
            ai_availability: n.ai_availability.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            plugins: n
                .plugins
                .iter()
                .map(|p| PluginInfo {
                    name: p.name.clone(),
                    description: p.description.clone(),
                    filename: p.filename.clone(),
                })
                .collect(),
            connection: Connection {
                effective_type: n.connection.effective_type.clone(),
                rtt: n.connection.rtt,
                downlink: n.connection.downlink,
                save_data: n.connection.save_data,
            },
            timezone: self.raw.intl.time_zone.clone(),
            intl_locale: self.raw.intl.locale.clone(),
        }
    }

    fn device_oracle_json(&self) -> String {
        // The captured deviceOracle block, passed through verbatim to the canvas/WebGL/audio bindings.
        serde_json::to_string(&self.raw.device_oracle).unwrap_or_else(|_| "{}".to_string())
    }

    fn computed_style_properties(&self) -> Option<std::sync::Arc<Vec<String>>> {
        // Through the persona's OWN bundle, as `theme` is: which names exist is the worn device's.
        crate::computed_style::for_bundle(self.bundle_dir())
    }

    fn css_named_properties(&self) -> Option<std::sync::Arc<Vec<String>>> {
        // Through the persona's OWN bundle, as the computed-style list is: which names exist is the worn build's.
        crate::css_named_properties::for_bundle(self.bundle_dir())
    }

    fn speech_voices(&self) -> Vec<spec::SpeechVoice> {
        self.raw.speech.voices.clone()
    }

    fn webrtc_offer(&self) -> Option<std::sync::Arc<spec::WebrtcOffer>> {
        // Through the persona's OWN bundle, as `codecs` is: the codecs an offer lists are the worn build's.
        crate::webrtc_offer::for_bundle(self.bundle_dir())
    }

    fn webrtc_capabilities(&self) -> Option<std::sync::Arc<spec::WebrtcCapabilities>> {
        // Through the persona's OWN bundle, as the offer is: the codecs listed are the worn build's.
        crate::webrtc_capabilities::for_bundle(self.bundle_dir())
    }
}

// ---------------------------------------------------------------------------------------------------
// Raw captured-profile shape. Mirrors the bundle's `values.json` exactly (camelCase keys). `#[serde(default)]`
// everywhere so a profile that omits a field degrades gracefully instead of failing to parse.
// ---------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct RawProfile {
    #[serde(default)]
    nav: RawNav,
    #[serde(default)]
    screen: RawScreen,
    #[serde(default)]
    win: RawWin,
    #[serde(default)]
    memory: Option<RawMemory>,
    #[serde(default)]
    intl: RawIntl,
    /// Opaque device-oracle replay block (WebGL/canvas/audio); passed through as JSON.
    #[serde(default, rename = "deviceOracle")]
    device_oracle: serde_json::Value,
    /// `window.chrome` as the rig recorded it. Only its KEY ORDER is read: the bodies
    /// belong to `core/korvus-webapi/src/modules/chrome_object.rs` and always did.
    #[serde(default)]
    chrome: RawChrome,
    /// `speechSynthesis.getVoices()`.
    #[serde(default)]
    speech: RawSpeech,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
struct RawSpeech {
    voices: Vec<spec::SpeechVoice>,
}

/// The `chrome` block of a capture's `values.json`, as `collect.js` writes it.
///
/// Only `keys` is consumed. The rest of the block (`hasRuntime`, the two `typeof`s, `app`'s key list)
/// is what the CAPTURE RIG records so that a human can see the object agrees with the engine's; it is
/// not a second definition of the object and nothing reads it into the surface.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
struct RawChrome {
    /// `Object.keys(chrome)`, in the device's own enumeration order.
    keys: Vec<String>,
}

/// `null` in a capture means "the browser does not expose this", which for a value type is the same
/// as its default. Serde treats the two differently by default and errors on the null; this bridges
/// them so a capture can record an absent member without failing the load.
fn null_as_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawNav {
    user_agent: String,
    platform: String,
    vendor: String,
    vendor_sub: String,
    app_name: String,
    app_code_name: String,
    product: String,
    product_sub: String,
    hardware_concurrency: u32,
    device_memory: f64,
    max_touch_points: u32,
    pdf_viewer_enabled: bool,
    cookie_enabled: bool,
    do_not_track: Option<String>,
    /// `navigator.cpuPerformance` (Chrome 152): the browser's CPU tier, 0 (unknown) to 4.
    cpu_performance: u16,
    languages: Vec<String>,
    /// The `Accept-Language` the captured browser put on ITS OWN request, read back off the capture
    /// host — the wire half of `languages`. Empty when the capture predates the field; see
    /// [`spec::NavigatorProfile::accept_language`] for why it is recorded rather than computed.
    accept_language: String,
    /// `navigator.globalPrivacyControl`. `null`/absent in a capture from a browser that does not
    /// expose the member (Chrome, Edge, Opera), `true` in Brave's — and it decides `Sec-GPC` on the
    /// wire, so the two halves cannot disagree.
    global_privacy_control: Option<bool>,
    accept_navigation: Option<String>,
    /// `"blocked"` when the browser blocks third-party cookies (Brave), `"allowed"` otherwise; absent in
    /// a bundle that predates the field, which reads as allowed (Chromium's default).
    third_party_cookies: Option<String>,
    /// `navigator.keyboard.getLayoutMap()`'s `[code, key]` entries in iteration order; `null`/absent when
    /// `navigator.keyboard` is `null` (Brave).
    keyboard_layout_map: Option<Vec<(String, String)>>,
    plugins: Vec<RawPlugin>,
    user_agent_data: RawUaData,
    /// `null` when the browser does not expose `navigator.connection` at all -- which Brave does
    /// not, and which is a RECORDABLE FACT about the device rather than a hole in the capture. The
    /// first Linux bundle failed to load on exactly this: the mac profile happened to have a
    /// connection block, the loader was written against that, and a browser that hides the member
    /// produced `invalid type: null, expected struct RawConnection`.
    #[serde(default, deserialize_with = "null_as_default")]
    connection: RawConnection,
    /// `navigator.permissions.query` states by permission name, as the source browser answered them.
    permissions: std::collections::BTreeMap<String, String>,
    /// `navigator.storage.estimate()` as the source browser resolved it.
    storage_estimate: Option<RawStorageEstimate>,
    /// `navigator.mediaDevices` as the source browser answered it, unprompted.
    #[serde(default, deserialize_with = "null_as_default")]
    media_devices: RawMediaDevices,
    /// The built-in AI APIs' `availability()` answers by interface ([`spec::ClientHints::ai_availability`]); absent
    /// in a bundle that predates the field, or from a browser without the APIs (Brave).
    #[serde(default)]
    ai_availability: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
struct RawMediaDevices {
    /// `enumerateDevices()`'s `kind`s, in the order it listed them.
    kinds: Vec<String>,
    /// `getSupportedConstraints()`, member by member. WebIDL converts a dictionary to an object with
    /// its members in lexicographic order, so the sorted map IS the order the browser returns.
    supported: std::collections::BTreeMap<String, bool>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
struct RawStorageEstimate {
    quota: f64,
    usage: f64,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
struct RawBrand {
    brand: String,
    version: String,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
struct RawPlugin {
    name: String,
    description: String,
    filename: String,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
// `rename_all` matters here now: every field was one word until `high_entropy` arrived, so its
// absence was invisible and the nested block silently defaulted to empties.
#[serde(default, rename_all = "camelCase")]
struct RawUaData {
    brands: Vec<RawBrand>,
    mobile: bool,
    platform: String,
    /// `getHighEntropyValues(...)`, NESTED INSIDE `userAgentData` -- the shape the browser itself
    /// has, and the shape the collector records.
    ///
    /// This lived on `nav` as a sibling called `uaHighEntropy`, which no capture has ever written.
    /// serde filled it with `Default` and said nothing, so `uaFullVersion`, `architecture`,
    /// `bitness` and the whole `fullVersionList` arrived as empty strings and an empty vec, and a
    /// page calling `getHighEntropyValues(["uaFullVersion"])` -- which is most of the bot checks
    /// that ask at all -- got `""` back from a browser claiming to be Chrome 151. Nothing failed;
    /// the values were simply not there.
    high_entropy: RawHighEntropy,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawHighEntropy {
    architecture: String,
    bitness: String,
    full_version_list: Vec<RawBrand>,
    model: String,
    platform_version: String,
    ua_full_version: String,
    wow64: bool,
    form_factors: Vec<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawConnection {
    effective_type: String,
    rtt: u32,
    downlink: f64,
    save_data: bool,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawScreen {
    width: u32,
    height: u32,
    color_depth: u32,
    pixel_depth: u32,
    avail_width: u32,
    avail_height: u32,
    avail_left: i32,
    avail_top: i32,
    /// `screen.orientation` as the capture recorded it. The struct-level `serde(default)` covers a
    /// bundle taken before the field existed, and an absent object leaves `ty` empty -- which is what
    /// `spec::ScreenProfile` reads as "not captured".
    orientation: RawScreenOrientation,
    /// The display refresh rate (Hz) the capture measured; absent in older bundles.
    refresh_rate: f64,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawScreenOrientation {
    // `type` is a Rust keyword, so the JSON key is named explicitly rather than derived.
    #[serde(rename = "type")]
    ty: String,
    angle: u32,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawMemory {
    // The capture spells these with an uppercase "JS", which `rename_all = "camelCase"` cannot
    // produce (it yields `totalJsHeapSize`). Named explicitly so the values are not silently zero --
    // which is exactly how they read before this, and a zero heap is itself a tell.
    #[serde(rename = "jsHeapSizeLimit")]
    js_heap_size_limit: f64,
    #[serde(rename = "totalJSHeapSize")]
    total_js_heap_size: f64,
    #[serde(rename = "usedJSHeapSize")]
    used_js_heap_size: f64,
    /// `performance.memory` read by an empty document's first script, in a fresh profile.
    #[serde(rename = "emptyDocument")]
    empty_document: Option<RawEmptyHeap>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
struct RawEmptyHeap {
    #[serde(rename = "totalJSHeapSize")]
    total_js_heap_size: f64,
    #[serde(rename = "usedJSHeapSize")]
    used_js_heap_size: f64,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawWin {
    device_pixel_ratio: f64,
    /// Window geometry. The struct-level `serde(default)` covers a capture that lacks them.
    inner_width: u32,
    inner_height: u32,
    outer_width: u32,
    outer_height: u32,
    screen_x: i32,
    screen_y: i32,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawIntl {
    time_zone: String,
    locale: String,
}

/// The shipped bundle with one or two fields replaced — for a test that genuinely needs a particular
/// screen or user-agent and must not invent the other twenty values to get it.
///
/// WHY THIS EXISTS. Ninety-three test files used to declare their own `impl Persona`, and the values
/// in them were invented: `"KorvusUA/1.0"`, a bare `"Mozilla/5.0"`, `deviceMemory: 16.0` (a number
/// Chrome cannot report, and the pre-correction value of the one field the bundle hand-edits). A test
/// that only cares about a screen size was, without meaning to, also asserting a device that does not
/// exist -- and the WPT runner and the bench were driving the engine with an EMPTY persona, so every
/// conformance number this project has was measured on a browser whose `navigator.userAgent` was `""`.
///
/// Delegating to the bundle makes the default correct and the override explicit: whatever a test does
/// not name, it gets from the one device this build claims to be.
#[derive(Debug, Clone)]
pub struct Overridden {
    base: CapturedPersona,
    navigator: Option<NavigatorProfile>,
    screen: Option<ScreenProfile>,
    user_agent: Option<String>,
    no_oracle: bool,
    hardware: Option<(u32, f64)>,
    /// korvus's own identity (goal 1020 D11): its version, which the UA and the brands carry (`Overridden::korvus`).
    korvus: Option<String>,
    /// A BCP 47 tag: `navigator.languages` and ICU's default locale.
    locale: Option<String>,
    /// An IANA zone ("" = this machine's own).
    timezone: Option<String>,
    /// A persona document's device oracle (its GPU strings and WebGL set over the bundle's; `document::device_oracle`).
    device_oracle: Option<String>,
}

impl Overridden {
    pub fn new() -> Self {
        Self::on(CapturedPersona::default())
    }

    /// Overrides on a specific base -- the one `KORVUS_BUNDLE` selected, rather than the embedded
    /// bundle [`new`](Self::new) always starts from.
    pub fn on(base: CapturedPersona) -> Self {
        Self { base, navigator: None, screen: None, user_agent: None, no_oracle: false, hardware: None, korvus: None, locale: None, timezone: None, device_oracle: None }
    }
    pub fn navigator(mut self, n: NavigatorProfile) -> Self {
        self.navigator = Some(n);
        self
    }
    pub fn screen(mut self, s: ScreenProfile) -> Self {
        self.screen = Some(s);
        self
    }
    /// Replace ONLY the user-agent, keeping every other navigator value from the bundle.
    ///
    /// Exists for one real case: the vendored an agent-browser reference conformance suite decides whether it is
    /// running under its own runner with `navigator.userAgent.startsWith("an agent-browser reference/")`, and takes
    /// a different code path when it is not. That UA is not an invented value the way the other test
    /// stubs' were -- it is load-bearing, and replacing it with the bundle's Chrome UA silently sent
    /// the harness down its "some other browser" branch.
    pub fn user_agent(mut self, ua: &str) -> Self {
        self.user_agent = Some(ua.to_string());
        self
    }
    /// Serve no device oracle — the shipped device in every other respect, but with no recorded
    /// GPU, canvas or audio. What "a persona that captured no device" actually means, as opposed to
    /// a persona that captured no anything.
    pub fn without_device_oracle(mut self) -> Self {
        self.no_oracle = true;
        self
    }
    /// The machine's own `navigator.hardwareConcurrency` and `navigator.deviceMemory`, as Chrome computes them on
    /// `host`, in place of the capture's (owner 2026-10-12: the hardware a page sees is this PC's, never another's;
    /// a persona captured on one machine and worn on another keeps every value but these). A fact the host does not
    /// give keeps the capture's.
    pub fn hardware_of(mut self, host: &spec::coherence::HostFacts) -> Self {
        let n = self.base.navigator();
        self.hardware = Some((
            host.cores.unwrap_or(n.hardware_concurrency),
            host.memory_mib.map(spec::coherence::chrome_device_memory).unwrap_or(n.device_memory),
        ));
        self
    }
    /// A device's own cores and memory -- a persona document's (`document::wear`) -- in place of this machine's.
    pub fn with_hardware(mut self, cores: u32, memory_gb: f64) -> Self {
        self.hardware = Some((cores, memory_gb));
        self
    }
    /// A device oracle in place of the bundle's: a persona document's GPU strings and WebGL set (`document::wear`).
    pub fn with_device_oracle(mut self, json: String) -> Self {
        self.device_oracle = Some(json);
        self
    }
    /// korvus's own identity over the build (goal 1020 D11): Chrome's UA tokens and `Korvus/<version>` after them (as
    /// Edge adds `Edg/`), "Korvus" in the client hints' brands where Google Chrome's was, and `navigator.webdriver`
    /// true.
    pub fn korvus(mut self, version: &str) -> Self {
        self.korvus = Some(version.to_string());
        self
    }
    /// `navigator.languages` (the tag, then its language) and ICU's default locale from one BCP 47 tag.
    pub fn locale(mut self, tag: &str) -> Self {
        self.locale = Some(tag.to_string());
        self
    }
    /// The IANA zone the page's clock reads ("": this machine's own).
    pub fn timezone(mut self, zone: &str) -> Self {
        self.timezone = Some(zone.to_string());
        self
    }
    pub fn boxed(self) -> Box<dyn Persona> {
        Box::new(self)
    }
}

impl Default for Overridden {
    fn default() -> Self {
        Self::new()
    }
}

/// **The** answer to "which identity does this process wear?", read from the environment.
///
/// The persona plugged in ([`CapturedPersona::plugged_in`]; none is an error, and korvus does not start);
/// `$KORVUS_PERSONA=tell` wears
/// that same bundle with two deliberate tells -- a `HeadlessChrome` UA and no GPU -- for one caller,
/// a detector's negative control. A detector that passes the subject but does not flag
/// the control has not detected anything.
///
/// `tell` replaced `KORVUS_PERSONA=null`, which selected an EMPTY persona (`userAgent: ""`,
/// `platform: ""`, no TLS profile). That is not a neutral identity but an impossible one, the boot
/// gate refuses it, and as a control it proved less than this does -- a browser nobody could mistake
/// for a browser says nothing about whether the detector catches a plausible one.
///
/// WHY IT LIVES HERE (W9.7/W9.9). It was a private `build_persona` inside `korvus/src/main.rs`, which
/// meant every OTHER binary had to answer the question again, and they answered it differently:
/// a fetch adapter wore `CapturedPersona::default()` -- the EMBEDDED bundle -- so a WPT run with
/// `$KORVUS_BUNDLE` set silently scored a device the operator had not selected. The identity a process
/// wears is the persona crate's fact; a binary asks for it, it does not decide it.
///
/// Returns the error rather than exiting so each binary reports it in its own voice; an unresolvable
/// bundle id is an error everywhere, never a quiet fallback to the embedded one.
pub fn from_env() -> Result<Box<dyn Persona>, String> {
    let host = spec::coherence::HostFacts::measure();
    let worn = match DOCUMENT.get() {
        // an identity a plugin answered (or `korvus --wear`): the build it names, its device over that build's
        Some(doc) => document::wear(doc, CapturedPersona::for_document(doc)?, &host),
        // no identity plugin answered: korvus's own identity, on this machine's build, hardware, zone and locale
        None => Overridden::on(CapturedPersona::plugged_in(host_os())?).hardware_of(&host).korvus(KORVUS_VERSION).locale(&host_locale()).timezone(""),
    };
    match std::env::var("KORVUS_PERSONA").as_deref() {
        Ok("tell") => {
            // The real UA with one word changed, so everything else stays coherent and the boot gate
            // lets it through: the OS family still matches the host, the major still matches the
            // declared TLS preset. Only the tell is wrong.
            let ua = Persona::navigator(&worn).user_agent.replace("Chrome/", "HeadlessChrome/");
            Ok(worn.user_agent(&ua).without_device_oracle().boxed())
        }
        Ok(other) if !other.is_empty() => {
            Err(format!("KORVUS_PERSONA={other:?} is not a mode; the only one is `tell`"))
        }
        _ => Ok(worn.boxed()),
    }
}

/// korvus's version, which its own identity's UA token and brand carry.
pub const KORVUS_VERSION: &str = env!("CARGO_PKG_VERSION");

/// This machine's OS as the persona format names it (`linux`, `macos`, `windows`).
pub fn host_os() -> &'static str {
    match std::env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        _ => "linux",
    }
}

/// A persona-format OS as `spec::coherence::os_family_from_ua` names it.
fn ua_family(os: &str) -> &str {
    if os == "macos" {
        "mac"
    } else {
        os
    }
}

/// This machine's locale as a BCP 47 tag: `$KORVUS_LOCALE` (a context's), else the POSIX locale (`LC_ALL`,
/// `LC_MESSAGES`, `LANG`: `ko_KR.UTF-8` is `ko-KR`), else `en-US`.
pub fn host_locale() -> String {
    if let Ok(l) = std::env::var("KORVUS_LOCALE") {
        if !l.trim().is_empty() {
            return l.trim().to_string();
        }
    }
    for k in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        let Ok(v) = std::env::var(k) else { continue };
        let tag = v.split(['.', '@']).next().unwrap_or("").replace('_', "-");
        if !tag.is_empty() && tag != "C" && tag != "POSIX" {
            return tag;
        }
    }
    "en-US".to_string()
}

/// `navigator.languages` for one tag, as Chrome lists a single UI language: the tag, then its language alone.
pub fn languages_of(tag: &str) -> Vec<String> {
    let lang = tag.split('-').next().unwrap_or(tag);
    if lang == tag || lang.is_empty() {
        vec![tag.to_string()]
    } else {
        vec![tag.to_string(), lang.to_string()]
    }
}

/// `Accept-Language` for `languages`, as Chrome writes it (`net::HttpUtil::GenerateAcceptLanguageHeader`): the first
/// bare, then each with a q-value a tenth lower than the one before, never below 0.1.
pub fn accept_language_of(languages: &[String]) -> String {
    let mut q = 10;
    let mut out: Vec<String> = Vec::with_capacity(languages.len());
    for l in languages {
        out.push(if q == 10 { l.clone() } else { format!("{l};q=0.{q}") });
        if q > 1 {
            q -= 1;
        }
    }
    out.join(",")
}

static DOCUMENT: std::sync::OnceLock<document::Document> = std::sync::OnceLock::new();

/// Wear `doc`, an identity a plugin answered, in every engine this process builds from here on ([`from_env`]). Set
/// once, before the first engine: a process wears one persona. (Not an environment variable: every `KORVUS_*` one keys the template cache, and a
/// document per value would record its templates afresh.)
pub fn wear_document(doc: document::Document) -> Result<(), String> {
    DOCUMENT.set(doc).map_err(|d| format!("this process already wears a persona document, not {:?}", d.name))
}

/// Wear a captured bundle the way [`from_env`] does for the engine: this machine's hardware over the
/// bundle's (its cores and memory as Chrome counts them), so the assembled persona is coherent on the
/// host it runs on. A bundle captured on another machine -- a different core count -- is otherwise
/// refused by the engine's host audit. This is the seam the per-bundle tests wear every bundle in the tree through, so they grade a
/// foreign-host capture the same assembled persona production would run.
pub fn worn_on_host(base: CapturedPersona) -> Box<dyn Persona> {
    let host = spec::coherence::HostFacts::measure();
    Overridden::on(base).hardware_of(&host).boxed()
}

impl Persona for Overridden {
    fn source_stamp(&self) -> u64 {
        self.base.source_stamp()
    }
    fn seed_surface(&self, realm: spec::SurfaceRealm, surface: std::sync::Arc<spec::RealmSurface>) {
        // an override changes values, not the surface (see `surface`): the base's
        self.base.seed_surface(realm, surface)
    }
    fn navigator(&self) -> NavigatorProfile {
        let mut n = self.navigator.clone().unwrap_or_else(|| self.base.navigator());
        if let Some(ua) = &self.user_agent {
            n.user_agent = ua.clone();
        }
        if let Some((cores, memory)) = self.hardware {
            n.hardware_concurrency = cores;
            n.device_memory = memory;
        }
        if let (Some(v), None) = (&self.korvus, &self.user_agent) {
            n.user_agent = format!("{} Korvus/{v}", n.user_agent);
        }
        if let Some(tag) = &self.locale {
            n.languages = languages_of(tag);
            // the wire half of the same list: left at the capture's, a page read `en-US` in `navigator.languages` while
            // its own requests said `ko-KR` (measured 2026-10-22 off a TLS/h2 echo, `--identity korvus` on a ko capture)
            n.accept_language = accept_language_of(&n.languages);
        }
        n
    }
    fn screen(&self) -> ScreenProfile {
        self.screen.clone().unwrap_or_else(|| self.base.screen())
    }
    fn memory(&self) -> MemoryProfile {
        self.base.memory()
    }
    fn transport_profile(&self) -> TlsProfile {
        self.base.transport_profile()
    }
    fn navigator_extra(&self) -> NavigatorExtra {
        self.base.navigator_extra()
    }
    fn client_hints(&self) -> ClientHints {
        let mut ch = self.base.client_hints();
        if let Some(v) = &self.korvus {
            // in Google Chrome's place, as Edge and Brave list themselves: korvus is a Chromium, not Google Chrome
            let major = v.split('.').next().unwrap_or(v).to_string();
            for (list, version) in [(&mut ch.brands, major), (&mut ch.full_version_list, v.clone())] {
                match list.iter_mut().find(|b| b.brand == "Google Chrome") {
                    Some(b) => *b = Brand { brand: "Korvus".into(), version },
                    None => list.push(Brand { brand: "Korvus".into(), version }),
                }
            }
        }
        if let Some(tz) = &self.timezone {
            ch.timezone = tz.clone();
        }
        if let Some(tag) = &self.locale {
            ch.intl_locale = tag.clone();
        }
        ch
    }
    fn webdriver(&self) -> bool {
        self.korvus.is_some()
    }
    fn device_oracle_json(&self) -> String {
        if self.no_oracle {
            "{}".to_string()
        } else if let Some(o) = &self.device_oracle {
            o.clone()
        } else {
            self.base.device_oracle_json()
        }
    }
    fn known_incoherences(&self) -> Vec<String> {
        // An override can only make the device LESS coherent, and a test that deliberately builds a
        // Frankenstein wants the audit to say so. It inherits the bundle's accounted-for list and
        // nothing more.
        self.base.known_incoherences()
    }
    fn surface(&self, realm: spec::SurfaceRealm) -> Option<std::sync::Arc<spec::RealmSurface>> {
        // An override changes VALUES. `KORVUS_PERSONA=tell` puts `HeadlessChrome/` in the UA of an
        // otherwise real device; it does not change which interfaces that device has, and inventing a
        // surface to go with it would make the control prove something else.
        self.base.surface(realm)
    }

    fn codecs(&self) -> Option<std::sync::Arc<spec::CodecGrid>> {
        // Same reason, one artifact over: an override changes VALUES, and which decoders the captured
        // build was compiled with is not one of them.
        self.base.codecs()
    }

    fn theme(&self) -> Option<spec::ThemeProfile> {
        // An override changes VALUES; the platform the captured build draws with is not one of them.
        self.base.theme()
    }

    fn computed_style_properties(&self) -> Option<std::sync::Arc<Vec<String>>> {
        self.base.computed_style_properties()
    }

    fn css_named_properties(&self) -> Option<std::sync::Arc<Vec<String>>> {
        self.base.css_named_properties()
    }

    fn speech_voices(&self) -> Vec<spec::SpeechVoice> {
        self.base.speech_voices()
    }

    fn webrtc_offer(&self) -> Option<std::sync::Arc<spec::WebrtcOffer>> {
        self.base.webrtc_offer()
    }

    fn webrtc_capabilities(&self) -> Option<std::sync::Arc<spec::WebrtcCapabilities>> {
        self.base.webrtc_capabilities()
    }

    // An override changes values; these four are the captured build's, so they are the base's (left to the trait's
    // defaults, an overridden persona lost them).
    fn farbling(&self) -> Option<spec::FarblingPolicy> {
        self.base.farbling()
    }

    fn hidden_objects(&self) -> Vec<spec::HiddenObject> {
        self.base.hidden_objects()
    }

    fn window_namespaces(&self) -> Vec<spec::WindowNamespace> {
        self.base.window_namespaces()
    }

    fn chrome_object_keys(&self) -> Vec<String> {
        self.base.chrome_object_keys()
    }
}

#[cfg(test)]
mod farbling_load_tests {
    use super::*;
    use spec::Persona;

    #[test]
    fn the_shipped_chrome_persona_does_not_farble() {
        assert!(CapturedPersona::shipped().farbling().is_none(), "Chrome serves its scalars static");
    }

    #[test]
    fn a_persona_folder_carrying_farbling_json_farbles() {
        // The mechanism stays for a farbling persona plugin (Brave's, should it come back): a folder's farbling.json is
        // read whole. Brave 151's captured policy, kept as test data: hwc uniform [2,12] (N=12 unfarbled), deviceMemory
        // uniform over {4,8,16,32}. Read from the file, not invented here (I4).
        let dir = super::plugin_tests::temp_persona("farbling", |d| {
            std::fs::write(d.join("farbling.json"), include_str!("../testdata/brave-151-farbling.json")).unwrap()
        });
        let pol = CapturedPersona::from_dir(&dir).expect("the folder loads").farbling().expect("a farbling.json farbles");
        assert_eq!(pol.hardware_concurrency_max, Some(12), "hwc N from the `unfarbled` key");
        assert_eq!(pol.device_memory_set, vec![4.0, 8.0, 16.0, 32.0], "deviceMemory support from `counts`");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bundle_without_farbling_json_serves_scalars_static() {
        // No farbling artifact -> None -> the getter serves the static captured scalar, which is what a
        // non-farbling browser (Chrome/Edge/Opera) does.
        let p = CapturedPersona::from_json(SHIPPED_VALUES).expect("values parse");
        assert!(p.farbling().is_none(), "no farbling_json means no farbling");
    }
}

#[cfg(test)]
mod plugin_tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// A persona folder in a fresh temporary directory -- the shipped bundle's values and manifest -- then `edit`.
    pub(super) fn temp_persona(tag: &str, edit: impl FnOnce(&Path)) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("korvus-persona-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("values.json"), SHIPPED_VALUES).unwrap();
        std::fs::write(dir.join("manifest.toml"), BUNDLE_MANIFEST).unwrap();
        edit(&dir);
        dir
    }

    fn with_release(release: &str) -> impl FnOnce(&Path) + '_ {
        move |d: &Path| std::fs::write(d.join("values.json"), SHIPPED_VALUES.replace(build_chrome(), release)).unwrap()
    }

    #[test]
    fn the_shipped_persona_is_the_release_this_build_is_for() {
        let v: serde_json::Value = serde_json::from_str(SHIPPED_VALUES).unwrap();
        assert_eq!(chromium_version(&v).as_deref(), Some(build_chrome()), "tools/version/chrome.toml and the shipped bundle");
    }

    #[test]
    fn a_persona_from_another_milestone_is_refused() {
        let dir = temp_persona("other", with_release("999.0.0.1"));
        let e = CapturedPersona::from_dir(&dir).unwrap_err();
        assert!(e.contains("captured from Chromium 999.0.0.1") && e.contains(build_chrome()), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_patch_release_of_the_same_milestone_is_accepted() {
        let patch = format!("{}.0.9999.1", milestone(build_chrome()));
        let dir = temp_persona("patch", with_release(&patch));
        assert!(CapturedPersona::from_dir(&dir).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A real Windows persona bundle in this checkout, for the selection tests.
    fn a_windows_bundle_dir() -> std::path::PathBuf {
        let bundles = Path::new(env!("CARGO_MANIFEST_DIR")).join("bundles");
        std::fs::read_dir(&bundles)
            .unwrap_or_else(|e| panic!("read {}: {e}", bundles.display()))
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.join("values.json").is_file())
            .find(|p| {
                std::fs::read_to_string(p.join("values.json"))
                    .map(|s| spec::coherence::os_family_from_ua(&s) == Some("windows"))
                    .unwrap_or(false)
            })
            .expect("a Windows persona bundle under plugins/korvus-persona/bundles")
    }

    /// `a_windows_bundle_dir`'s values.json and manifest.toml text.
    fn a_windows_bundle_text() -> (String, String) {
        let dir = a_windows_bundle_dir();
        (
            std::fs::read_to_string(dir.join("values.json")).expect("windows values.json"),
            std::fs::read_to_string(dir.join("manifest.toml")).expect("windows manifest.toml"),
        )
    }

    #[test]
    fn nothing_plugged_in_refuses_and_says_where_it_looked() {
        let exe = std::env::temp_dir().join(format!("korvus-no-persona-{}", std::process::id())).join("korvus");
        let e = CapturedPersona::resolve_plugin(None, None, Some(&exe), "windows").unwrap_err();
        assert!(e.starts_with("no persona plugged in"), "{e}");
        assert!(e.contains(&exe.parent().unwrap().join("personas").display().to_string()), "{e}");
    }

    #[test]
    fn a_personas_folder_beside_the_executable_is_plugged_in() {
        // A release ships its builds beside the executable; the Windows one is worn for a Windows identity without
        // `--persona`.
        let root = std::env::temp_dir().join(format!("korvus-release-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let folder = root.join("personas").join("chrome");
        std::fs::create_dir_all(&folder).unwrap();
        let (values, manifest) = a_windows_bundle_text();
        std::fs::write(folder.join("values.json"), values).unwrap();
        std::fs::write(folder.join("manifest.toml"), manifest).unwrap();
        let p = CapturedPersona::resolve_plugin(None, None, Some(&root.join("korvus")), "windows").expect("the release's persona");
        assert_eq!(p.bundle_dir(), Some(folder.as_path()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_development_build_finds_each_os_s_build_in_its_checkout() {
        // The checkout's bundles/ holds this milestone's Linux, macOS and Windows builds: an identity's OS names one.
        let exe = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/release/korvus");
        for (os, family) in [("linux", "linux"), ("windows", "windows"), ("macos", "mac")] {
            let p = CapturedPersona::resolve_plugin(None, None, Some(&exe), os).unwrap_or_else(|e| panic!("{os}: {e}"));
            assert_eq!(spec::coherence::os_family_from_ua(&Persona::navigator(&p).user_agent), Some(family), "{os}'s build");
        }
    }

    #[test]
    fn an_os_s_build_is_its_first_by_capture_id_and_an_os_without_one_is_refused() {
        let root = std::env::temp_dir().join(format!("korvus-builds-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let win_dir = a_windows_bundle_dir();
        let (values, manifest) = a_windows_bundle_text();
        // the capture id the engine actually parses from this bundle, so folder "b" carries a distinct one
        let id = CapturedPersona::from_dir(&win_dir).expect("windows bundle loads").bundle().capture.id;
        assert!(manifest.contains(&id), "manifest must stamp the capture id {id} so folder b can differ");
        for (name, m) in [("b", manifest.replace(&id, &format!("{id}-b"))), ("a", manifest.clone())] {
            std::fs::create_dir_all(root.join(name)).unwrap();
            std::fs::write(root.join(name).join("values.json"), &values).unwrap();
            std::fs::write(root.join(name).join("manifest.toml"), m).unwrap();
        }
        let picked = CapturedPersona::pick_in(&root, "windows").unwrap().expect("a windows build").bundle().capture.id;
        assert_eq!(picked, id, "the first capture id, every time: no seed, no chance");
        let e = CapturedPersona::pick_in(&root, "macos").unwrap_err();
        assert!(e.contains("no macos build"), "{e}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn korvus_s_own_identity_says_it_is_korvus() {
        let base = CapturedPersona::default();
        let chrome_ua = Persona::navigator(&base).user_agent;
        let p = Overridden::on(base).korvus("152.0.0").locale("ko-KR").timezone("");
        assert_eq!(Persona::navigator(&p).user_agent, format!("{chrome_ua} Korvus/152.0.0"), "Chrome's tokens, then korvus's");
        let ch = p.client_hints();
        assert!(ch.brands.iter().any(|b| b.brand == "Korvus" && b.version == "152"), "{:?}", ch.brands);
        assert!(!ch.brands.iter().any(|b| b.brand == "Google Chrome"), "korvus is not Google Chrome: {:?}", ch.brands);
        assert!(ch.full_version_list.iter().any(|b| b.brand == "Korvus" && b.version == "152.0.0"));
        assert!(p.webdriver(), "a client automates the page, and korvus says so");
        assert_eq!(Persona::navigator(&p).languages, ["ko-KR", "ko"]);
        assert_eq!(ch.intl_locale, "ko-KR");
        assert!(!Overridden::on(CapturedPersona::default()).webdriver(), "a dressed identity is not korvus's");
    }

    #[test]
    fn a_tag_s_languages_are_the_tag_then_its_language() {
        assert_eq!(languages_of("en-GB"), ["en-GB", "en"]);
        assert_eq!(languages_of("ko"), ["ko"]);
    }

    #[test]
    fn a_locale_moves_the_header_with_the_languages() {
        // Chrome's own format: the first bare, then q falling by a tenth (`GenerateAcceptLanguageHeader`)
        let four: Vec<String> = ["ko-KR", "ko", "en-US", "en"].map(String::from).to_vec();
        assert_eq!(accept_language_of(&four), "ko-KR,ko;q=0.9,en-US;q=0.8,en;q=0.7");
        assert_eq!(accept_language_of(&languages_of("en-US")), "en-US,en;q=0.9");
        // one claim, two halves: an override of the locale is never a JS answer the wire contradicts
        let n = Persona::navigator(&Overridden::on(CapturedPersona::default()).locale("en-US"));
        assert_eq!(n.languages, ["en-US", "en"]);
        assert_eq!(n.accept_language, "en-US,en;q=0.9");
    }

    #[test]
    fn korvus_persona_dir_is_asked_first() {
        let dir = temp_persona("env", |_| {});
        let p = CapturedPersona::resolve_plugin(Some(dir.clone()), Some("no-such-bundle".into()), None, "windows").expect("the folder");
        assert_eq!(p.bundle_dir(), Some(dir.as_path()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
