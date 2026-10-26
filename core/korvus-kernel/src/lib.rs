//! `spec` — the shared contracts every Korvus crate agrees on.
//!
//! This crate holds **traits and plain data types only** — no implementations and no dependencies
//! on sibling crates. Everyone depends on `spec`, so there are no crate cycles, and the plugin
//! seams (persona, render, transport, instrumentation) are defined here once.
//!
//! These are skeletons. They exist so the seams are compiler-visible from day one;
//! persona/render/transport/behavior crates implement them incrementally in later goals.

// korvus-analysis Layer 1: the call recorder's event type and its non-blocking queue. Behind its feature so a
// production build compiles exactly the kernel it did before (the codegen-unit split moves with any change).
#[cfg(feature = "analysis")]
pub mod analysis;
pub mod coherence;
pub mod devtools; // What a DevTools client set on this page: the CDP overrides every layer reads through.
pub mod farble; // per-origin scalar farbling primitive (SessionSeed + uniform draws), beside FarblingPolicy.
pub mod kernel;
pub mod navtrace; // Where a navigation's time goes, phase by phase.

/// One coherent device profile — the identity the browser wears.
///
/// The host may return temporary hardcoded values, but every persona-visible value must
/// ultimately be resolved through this trait (every persona-visible value in `webapi` reads through it).
pub trait Persona: Send + Sync {
    fn navigator(&self) -> NavigatorProfile;
    fn screen(&self) -> ScreenProfile;
    /// `performance.memory`. Defaulted so a persona that captured no heap numbers simply does not
    /// expose the member -- the engine checks for zero rather than substituting its own heap.
    fn memory(&self) -> MemoryProfile {
        MemoryProfile::default()
    }
    /// Which TLS/H2 fingerprint this device emits on the wire (a `wreq-util` profile id in practice).
    fn transport_profile(&self) -> TlsProfile;

    /// Navigator scalars that are structurally constant across a browser family (vendor, product,
    /// productSub, appName, ...). The default is the Chrome-desktop set (identical across every real
    /// Chrome install); a non-Chrome persona overrides. These are STRUCTURAL, not per-device values —
    /// the per-device identity lives in [`NavigatorProfile`] / [`ScreenProfile`] / [`ClientHints`].
    fn navigator_extra(&self) -> NavigatorExtra {
        NavigatorExtra::default()
    }

    /// The UA Client-Hints (low + high entropy), plugin list, and NetworkInformation this device
    /// reports — all per-device, so the default is empty and a real persona MUST override it. Sourced
    /// from the captured profile so `navigator.userAgentData` / `navigator.plugins` /
    /// `navigator.connection` are coherent with the UA header and the transport fingerprint.
    fn client_hints(&self) -> ClientHints {
        ClientHints::default()
    }

    /// Brave's per-origin fingerprint farbling policy, as captured in the bundle's
    /// `farbling.json`. `None` for a browser that does not farble these signals at its default
    /// (Chrome/Edge/Opera), so that persona serves the static captured scalar -- which is exactly
    /// what those browsers do. The VALUES come from the bundle (I4), never from the engine, so the
    /// engine never carries a browser constant; a persona with no farbling artifact returns `None`.
    fn farbling(&self) -> Option<FarblingPolicy> {
        None
    }

    /// `navigator.webdriver`: true for korvus's own identity, which says a client automates the page (goal 1020
    /// D11); false for an identity a plugin dressed (a captured browser, used by a person).
    fn webdriver(&self) -> bool {
        false
    }

    /// The device-oracle replay data (WebGL getParameter/extensions, canvas `toDataURL`, audio
    /// fingerprint) as a JSON string, read once by the canvas/WebGL/audio bindings. Returned as opaque
    /// JSON (not a typed struct) because WebGL's getParameter values are heterogeneous (ints, floats,
    /// pairs, strings) -- this is recorded replay, not a live rasterizer. Default `{}` = no oracle
    /// (canvas/WebGL fall back to inert stubs); a real persona serves its captured device.
    fn device_oracle_json(&self) -> String {
        "{}".to_string()
    }

    /// The cross-layer contradictions this persona has ALREADY ACCOUNTED FOR, by
    /// [`coherence::Incoherence`] id, each with a written reason and a fix path in its own manifest.
    ///
    /// WHY A PERSONA GETS TO SAY THIS. The audit is a hard gate -- an unaccounted contradiction stops
    /// the engine -- and a hard gate over a tree that is genuinely mixed would simply refuse to run.
    /// The alternative, warning, is what was there before: a line printed in one code path that
    /// nobody read. So the persona declares which disagreements it knows about, the audit fails on
    /// everything else, and closing a gap means deleting a declaration.
    ///
    /// It belongs on the persona because the persona is what the disagreement is ABOUT. The kernel
    /// must not learn where a bundle keeps its manifest, and this default -- account for nothing --
    /// is the right one for a persona with no bundle behind it.
    fn known_incoherences(&self) -> Vec<String> {
        Vec::new()
    }

    /// WHICH interfaces and members this device exposes in `realm`, and IN WHAT ORDER — the
    /// selector half of the one-table rule.
    ///
    /// The engine's tables carry the UNION of the Chromium family's rows; a row says HOW a member
    /// behaves. This says WHICH of those rows the device being worn actually has, because that is a
    /// fact about the device and not about the implementation. `install::installer` joins the two at
    /// context creation: a row the selector does not name is not installed, and the order it gives
    /// is the order `Object.getOwnPropertyNames` reports — which a page reads in three lines.
    ///
    /// It belongs on the persona for the same reason [`Persona::known_incoherences`] does: it is
    /// recorded in the bundle the persona was LOADED from, so swapping `KORVUS_BUNDLE` swaps it with
    /// everything else the bundle says. The default — "I do not know my surface" — is the right one
    /// for a persona with no bundle behind it, and the installer then falls back to the table's own
    /// rows, which is what a bare test runtime wants.
    fn surface(&self, _realm: SurfaceRealm) -> Option<std::sync::Arc<RealmSurface>> {
        None
    }

    /// The context snapshot: `surface` as a snapshot of a page realm wearing THIS persona carried it, already
    /// parsed -- what [`Persona::surface`] answers for `realm` from then on in this process, as if parsed here. A
    /// persona that parses its surface from a bundle caches it there; one with none has nothing to seed (the default).
    fn seed_surface(&self, _realm: SurfaceRealm, _surface: std::sync::Arc<RealmSurface>) {}

    /// WHAT THIS DEVICE ANSWERS to "can you play this?" — the recorded container/codec grid.
    ///
    /// Same split as [`Persona::surface`], one family of members down. The engine owns the GRAMMAR —
    /// the MIME parser, the `avc1.PPCCLL` / `vp09.PP.LL.DD` / `mp4a.40.N` parameter grammars, and the
    /// three different questions `canPlayType`, `MediaSource.isTypeSupported` and
    /// `MediaRecorder.isTypeSupported` actually ask — and the bundle owns the GRID those run over,
    /// because which decoders a build was compiled with is a fact about the build.
    ///
    /// It has to be the persona's or it is a leak: Opera 135's Linux build ships without the
    /// proprietary H.264/AAC decoders, so an engine that answered from one compiled-in grid told a
    /// page it could play `avc1.42E01E` while wearing a device that cannot — 274 observables, the
    /// largest single hard-coded difference measured in this goal.
    ///
    /// The default — "I do not know what I can play" — is the right one for a persona with no bundle
    /// behind it, and the three members then answer `""`/`false` for everything, which is what an
    /// engine with no device and no decoders should say.
    fn codecs(&self) -> Option<std::sync::Arc<CodecGrid>> {
        None
    }

    /// The PLATFORM THEME's geometry this device lays out with — a classic scrollbar's thickness and
    /// a drop-down's theme paddings.
    ///
    /// None of these is a font fact or a script-visible value, and none is the engine's to choose:
    /// a classic scrollbar takes client width from every scroll container, an overlay-scrollbar
    /// platform draws none, and a Windows or macOS build may answer differently again. So they are
    /// recorded in the bundle (`theme.json`, read back off layout by the capture rig) and read from
    /// the device being worn, never written as a constant.
    ///
    /// The default — "I do not know my theme" — is the right one for a persona with no bundle
    /// behind it, and the engine then reserves no scrollbar and adds no theme padding anywhere, which
    /// is what a platform with overlay scrollbars and unthemed controls does.
    fn theme(&self) -> Option<ThemeProfile> {
        None
    }

    /// The names `getComputedStyle(el)` enumerates, in the device's order. Chrome 150
    /// lists 474; the Chromium-151 browsers 475 -- `ruby-overhang` shipped in 151, measured headed on all
    /// four. `None` keeps the engine's table, which was recorded from Brave 151.
    fn computed_style_properties(&self) -> Option<std::sync::Arc<Vec<String>>> {
        None
    }

    /// The names a `CSSStyleDeclaration`'s NAMED-property enumerator lists on the device, in its order:
    /// the JS name of every CSS property and alias the build exposes, sorted by code
    /// unit -- Chrome 150 744, the Chromium-151 browsers 745. Which camel-cased, dashed and `webkit`-cased names a
    /// declaration answers as its own follows from it. `None` keeps the engine's own table.
    fn css_named_properties(&self) -> Option<std::sync::Arc<Vec<String>>> {
        None
    }

    /// `speechSynthesis.getVoices()` as the device answers it: stock Chrome lists its
    /// 19 Google voices, Edge its Microsoft voices, Brave none. The default is the empty list.
    fn speech_voices(&self) -> Vec<SpeechVoice> {
        Vec::new()
    }

    /// The device's own SDP offer for one receive-only audio and one receive-only video transceiver.
    /// Which codecs, payload types, header extensions and feedback a section lists
    /// is the BUILD's, as the codec grid of [`Persona::codecs`] is: measured on the four installed browsers,
    /// Chromium 151 adds RTCP XR and numbers its video payload types differently from Chrome 150, and
    /// Opera's Linux build offers no H.264. The engine writes the connection's own values into it. The
    /// default -- none -- refuses an offer with a media section, which is right for a persona with no
    /// bundle behind it.
    fn webrtc_offer(&self) -> Option<std::sync::Arc<WebrtcOffer>> {
        None
    }

    /// The device's `RTCRtpReceiver.getCapabilities(kind)` / `RTCRtpSender.getCapabilities(kind)` answers:
    /// libwebrtc's decoder and encoder factories, the BUILD's and the device's like
    /// the offer -- the receiver's video list adds decode-only codecs the offer never lists, the sender's is a
    /// different subset in a different order. The default -- none -- answers null for every kind, which is
    /// right for a persona with no bundle behind it.
    fn webrtc_capabilities(&self) -> Option<std::sync::Arc<WebrtcCapabilities>> {
        None
    }

    /// The objects this device hands back from a `navigator` member whose INTERFACE it does not put
    /// on the global — `navigator.brave` is the only one in the Chromium family today.
    ///
    /// WHY IT IS THE PERSONA'S TO SAY. "There is an object called `Brave` behind `navigator.brave`,
    /// and its `isBrave()` resolves true" is a fact about ONE BROWSER, and P2 says a fact about one
    /// browser does not live in engine source: swap the persona and the fingerprint must swap with
    /// it. The capture rig records that the MEMBER exists (`surface.json`'s `Navigator.brave`) but
    /// not the shape of the object it returns, so this is the persona crate's own knowledge, stated
    /// where `scripts/persona_literals.py` allows the name to be spelled.
    ///
    /// It is a split, not a hand-off: the persona says WHICH members exist and WHAT they answer; the
    /// BODY is still a native Rust callback built by the engine (invariant I3), which is why
    /// [`HiddenAnswer`] is a tiny closed vocabulary and not a script. The default — none — is right
    /// for a persona with no bundle behind it.
    fn hidden_objects(&self) -> Vec<HiddenObject> {
        Vec::new()
    }

    /// The proprietary NAMESPACE OBJECTS this browser puts on the window itself — `window.opr` and
    /// `window.g_opr` on Opera, and nothing else in the Chromium family today.
    ///
    /// Same split, same reason, one holder over: a capture records that the global NAME exists
    /// (`surface.json`'s `globals` list) and can record nothing about the OBJECT, because `collect.js`
    /// walks property descriptors and the object behind a data property is not one. So the shape is
    /// the persona crate's own knowledge, stated where `scripts/persona_literals.py` allows the name
    /// to be spelled — and `opr` is exactly the literal P2 forbids in `core/`.
    ///
    /// A NAMESPACE IS NOT AN INTERFACE, which is why this is not a table row. `window.opr` is a plain
    /// object whose `constructor` is `Object` and whose `[[Prototype]]` is `Object.prototype`; there
    /// is no `Opr` interface object on the global, no prototype chain, nothing for a one-table row to
    /// describe. Measured on the installed Opera 135.0.5973.133, 2026-09-13.
    ///
    /// The bodies stay native Rust callbacks (invariant I3) chosen from [`HiddenAnswer`]'s closed
    /// vocabulary, so a persona cannot ask for a body the engine does not have and nothing here is a
    /// script. The default — none — is right for every persona that is not Opera's.
    fn window_namespaces(&self) -> Vec<WindowNamespace> {
        Vec::new()
    }

    /// The own keys of `window.chrome`, in the order this device enumerates them.
    ///
    /// `Object.keys(chrome)` is read directly by detectors and string keys enumerate in insertion
    /// order, so this list IS a surface. It is not the same everywhere in the Chromium family, which
    /// only a fourth bundle could show: Brave 151, Chrome 150 and Edge 151 all record
    /// `[loadTimes, csi, app]` and Opera 135 records `[loadTimes, csi, search, app]` -- the extra key
    /// sitting THIRD, not appended.
    ///
    /// P3 exactly, one object down from an interface: the bundle says WHICH members and in what
    /// ORDER, the engine says HOW each one answers. A key this engine has no body for is a counted
    /// gap, never a silently dropped member, and a body the device does not list is not installed.
    ///
    /// Empty -- the default, and a persona with no bundle -- means "this device cannot say", and the
    /// engine then installs every body it has, which is the same reading `install::surface` gives a
    /// realm with no active capture.
    ///
    /// WHY `chrome` IS NOT A [`WindowNamespace`]: its members are not a closed vocabulary of trivial
    /// answers. `loadTimes()` and `csi()` are derived from the realm's own `PerfClock` and `app` is a
    /// nested object with its own key order and its own WebIDL-style arity errors. Those bodies are
    /// the engine's and stay there; only the key list is the device's.
    fn chrome_object_keys(&self) -> Vec<String> {
        Vec::new()
    }

    /// The context snapshot (slice 3): a stamp of what this persona is READ FROM at run time -- the files of
    /// a capture directory, by path, size and modification time -- for a cache keyed by the persona's content (a
    /// snapshot of the page realm). 0, the default, for a persona compiled into the binary: its build id covers it.
    /// The variables that choose and modify a persona (`KORVUS_*`) key the cache separately.
    fn source_stamp(&self) -> u64 {
        0
    }
}

/// A device's platform-theme geometry, in CSS px. See [`Persona::theme`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ThemeProfile {
    /// A classic scrollbar's thickness under `scrollbar-width: auto`; 0 on an overlay platform.
    pub scrollbar_thickness: f32,
    /// The same under `scrollbar-width: thin`.
    pub scrollbar_thin_thickness: f32,
    /// A themed drop-down `<select>`'s internal inline padding, both sides together.
    pub menu_list_inline_padding: f32,
    /// A themed drop-down `<select>`'s internal block padding, both sides together.
    pub menu_list_block_padding: f32,
    /// Which bundle's font names the engine measures with: [`font_map_key`] of the worn bundle's id.
    /// Font metrics are the capturing device's, so the page pipeline compiles every
    /// bundle's map; `0` picks the shipped one.
    pub font_map: u64,
    /// The minimum LOGICAL font size the worn browser's UI locale sets (`IDS_MINIMUM_LOGICAL_FONT_SIZE`: 10
    /// under a Korean UI, 6 under any other); `0` means Chrome's default, 6. A font size not written in an
    /// absolute unit computes no smaller (`font-size: 0.5em` computes `10px` there).
    pub min_logical_font_size: f32,
    /// The default labels of `<input type=submit>` and `<input type=reset>` in the worn browser's UI language
    /// (`IDS_FORM_SUBMIT_LABEL`, `IDS_FORM_RESET_LABEL`, [`form_button_labels_for`]); empty means English.
    pub submit_label: &'static str,
    pub reset_label: &'static str,
}

/// Chrome's default `submit` and `reset` button labels for a browser whose UI speaks `locale`: a browser string, so
/// a Korean UI's page shows "제출" where an English one shows "Submit" -- and lays the button out that wide.
pub fn form_button_labels_for(locale: &str) -> (&'static str, &'static str) {
    match locale.split(['-', '_']).next().unwrap_or("").to_ascii_lowercase().as_str() {
        "ko" => ("제출", "초기화"),
        "ja" => ("送信", "リセット"),
        "zh" => ("提交", "重置"),
        "de" => ("Senden", "Zurücksetzen"),
        "fr" => ("Envoyer", "Réinitialiser"),
        "es" => ("Enviar", "Restablecer"),
        _ => ("Submit", "Reset"),
    }
}

/// Chrome's minimum logical font size for a browser whose UI speaks `locale` (`ko-KR`): the one locale
/// `locale_settings_*.xtb` sets one for is Korean, at 10 (`locale_settings_ko.xtb`); every other takes the
/// default 6 (`locale_settings.grd`).
pub fn min_logical_font_size_for(locale: &str) -> f32 {
    let lang = locale.split(['-', '_']).next().unwrap_or("");
    if lang.eq_ignore_ascii_case("ko") {
        10.0
    } else {
        6.0
    }
}

/// FNV-1a 64 of a bundle id: the key [`ThemeProfile::font_map`] carries. `emit.py`'s `font_map_key`
/// computes the same number for the generated table, so the two must agree bit for bit.
pub fn font_map_key(bundle_id: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bundle_id.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// One proprietary namespace object a browser puts on the window. See [`Persona::window_namespaces`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowNamespace {
    /// The own-property name on the global (`"opr"`). The engine installs it ONLY when the active
    /// capture's own-globals list carries the same name, so the two cannot disagree.
    pub global: String,
    /// Its own members, in the order a page enumerates them. **Empty is a real answer**, not a
    /// missing one: real Opera's `window.opr` has no own properties at all.
    pub members: Vec<HiddenMethod>,
    /// The own property descriptor's `configurable` flag. `writable` and `enumerable` are true for
    /// every namespace measured so far, so they are not fields; this one is not, and the difference is
    /// observable (`delete window.g_opr` answers false and the object stays, `delete window.opr`
    /// removes it).
    pub configurable: bool,
}

/// One object a browser reaches through `navigator` without exposing its interface object on the
/// global. See [`Persona::hidden_objects`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HiddenObject {
    /// The `navigator` member that hands it back (`"brave"`). Matched against the member name the
    /// one-table row carries, so the two cannot drift.
    pub member: String,
    /// The name its hidden constructor and `Symbol.toStringTag` carry (`"Brave"`).
    pub interface: String,
    /// Its prototype's methods, in the order a page enumerates them.
    pub methods: Vec<HiddenMethod>,
}

/// One method on a [`HiddenObject`]'s prototype, or on a [`WindowNamespace`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HiddenMethod {
    /// The property key the member is installed under.
    pub name: String,
    /// The function's own `name`, when it is not the key. `None` means "named after its key".
    ///
    /// A Chromium PROPRIETARY namespace carries ANONYMOUS functions: `g_opr.scrap.name` is `""` on
    /// real Opera 135, exactly as `chrome.loadTimes.name` is `""` on real Chrome
    /// (`modules/chrome_object.rs` records that measurement and the one-line detect it costs — a
    /// function named after its slot prints `function scrap() { [native code] }` where the real
    /// browser prints `function () { [native code] }`).
    pub fn_name: Option<String>,
    /// `Function.prototype.length`.
    pub length: u32,
    pub answer: HiddenAnswer,
}

/// What a [`HiddenMethod`] answers. A CLOSED VOCABULARY on purpose: the persona names the member and
/// says which of the engine's native bodies serves it, and a body the engine does not have cannot be
/// requested. Widening this is adding a Rust body, never adding a script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HiddenAnswer {
    /// `Promise.resolve(true)`.
    ResolvedTrue,
    /// `undefined`, from a call that does nothing. Measured: real Opera 135's `g_opr.scrap()` answers
    /// `undefined`, and this engine has no Opera scrapbook behind it to do anything else.
    Undefined,
}

/// Which global a [`RealmSurface`] describes. The two are separate captures of the same browser and
/// they disagree — about member sets, about interface sets, and about the order of both.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum SurfaceRealm {
    Window,
    Worker,
}

impl SurfaceRealm {
    /// The bundle manifest artifact role that records this realm's surface.
    pub const fn artifact_role(self) -> &'static str {
        match self {
            SurfaceRealm::Window => "surface",
            SurfaceRealm::Worker => "worker-surface",
        }
    }
}

/// One interface's own members, as the capture recorded them: the prototype's, then the interface
/// object's, each IN ENUMERATION ORDER.
///
/// The two lists are kept apart rather than concatenated because a WebIDL constant is one NAME in two
/// places (`Node.ELEMENT_NODE` sits on the prototype AND on the interface object), so a single list
/// keyed by name cannot say which of the two a given entry means.
#[derive(Debug, Default, Clone)]
pub struct MemberOrder {
    pub members: Vec<String>,
    pub statics: Vec<String>,
    /// WHERE `constructor` SITS among the prototype's own keys, as the capture recorded it
    /// — an index into the FULL own-key sequence, of which [`Self::members`] is the same
    /// sequence with `constructor` taken out. So `ctor_index == members.len()` means last, `0` means
    /// first, and anything between means the interface has PARTIAL members: `constructor` is the last
    /// thing installed at interface-definition time, and a partial interface's members land after it.
    /// `Navigator` 32/66, `Performance` 13/18, `StorageManager` 2/4, `Screen` 9/11 on brave-151.
    ///
    /// `None` is a capture that does not record the position. Every `surface.json` in this tree does;
    /// **no `worker.surface.json` does** — the field was added to the window artifact only, so the
    /// worker realm keeps the placement it had. A position is never guessed here.
    pub ctor_index: Option<usize>,
}

/// One realm's recorded surface, reduced to the two questions the installer asks of it: which names
/// the global owns (in order), and which members each interface owns (in order).
///
/// Deliberately NOT the whole of `surface.json`. The descriptors, nativeness, function names and
/// lengths in that file are the GRADER's inputs — `tests/quality.rs` and `tests/worker_surface.rs`
/// read them from the bundle directly. What the installer needs is presence and sequence, and a
/// selector that carried more would invite a second definition of facts the tables already carry.
#[derive(Debug, Default)]
pub struct RealmSurface {
    /// The capture id this came from, for the audits that want to name it.
    pub capture_id: String,
    /// `Object.getOwnPropertyNames(globalThis)` as the capture recorded it.
    pub globals: Vec<String>,
    /// EXPOSED interface name (`WorkerNavigator`, not `Navigator`) -> its members.
    pub interfaces: std::collections::HashMap<String, MemberOrder>,
}

impl RealmSurface {
    /// Does this device expose a global with this name?
    pub fn has_global(&self, name: &str) -> bool {
        self.globals.iter().any(|g| g == name)
    }

    /// This interface's recorded members, if the capture records the interface at all. `None` means
    /// "this capture cannot speak for that name" — not "it has no members"; the installer falls back
    /// to the table's own rows there, which is what an interface the browser does not expose as a
    /// global (and an internal helper table) needs.
    pub fn members_of(&self, exposed_name: &str) -> Option<&MemberOrder> {
        self.interfaces.get(exposed_name)
    }
}

/// One container's recorded "can you play this?" answers: the answer with NO `codecs=` parameter, and
/// which codec spellings each of the three members accepted.
///
/// DATA, not a decision. Which spellings generalise to which others is the codec GRAMMAR's business
/// (`core/dom/media_codecs.rs` parses `avc1.PPCCLL`, `vp09.PP.LL.DD`, `mp4a.40.N`, `av01.P.LLT.DD`
/// and reduces these lists to canonical ids); this carries only what the visited browser answered.
#[derive(Debug, Default, Clone)]
pub struct CodecContainer {
    /// `<video>.canPlayType("<container>")` — `""`, `"maybe"` or `"probably"`.
    pub base_video: String,
    /// The same question asked of an `<audio>` element. Recorded separately because the capture rig
    /// asks both; identical in all four bundles in this tree today. Kept apart rather than folded
    /// into `base_video` so that a device on which the two DIFFER fails a gate instead of being
    /// silently answered with the video element's grid.
    pub base_audio: String,
    /// `MediaSource.isTypeSupported("<container>")`.
    pub base_ms: bool,
    /// `MediaRecorder.isTypeSupported("<container>")`.
    pub base_mr: bool,
    /// The codec spellings this container answered `"probably"` to through `<video>.canPlayType`.
    pub video: Vec<String>,
    /// …through `<audio>.canPlayType`.
    pub audio: Vec<String>,
    /// …`true` to through `MediaSource.isTypeSupported`.
    pub ms: Vec<String>,
    /// …`true` to through `MediaRecorder.isTypeSupported`.
    pub mr: Vec<String>,
    /// Spellings this container answered FALSE to through `MediaRecorder.isTypeSupported` whose
    /// FAMILY it accepts through another spelling. The WebM/Matroska muxer matches the family and
    /// stops for most codecs (`avc1`, `avc1.ZZZZZZ`, `VP8` all mux as WebM video), so the grammar
    /// accepts a family-match leniently -- but it rejects the bare family for a few (`hev1.1.6.L93.B0`
    /// muxes into Matroska on Windows, bare `hev1` does not). Those rejections are a fact of the
    /// recording, not a grammar rule, so they are carried to be denied. Only the discriminating ones
    /// (family in `mr`): a false spelling whose family is accepted nowhere is already denied.
    pub mr_reject: Vec<String>,
}

/// WHICH CONTAINERS AND CODECS THIS DEVICE CLAIMS, as its capture recorded them.
///
/// The sibling of [`RealmSurface`], one question over: the surface says which members the device HAS,
/// this says what one family of them ANSWERS. It is a fact about the device and not about the
/// implementation — Opera's Linux build ships without the proprietary H.264/AAC decoders and answers
/// `""` where Brave, Chrome and Edge answer `"probably"`, which is 274 observables on the four
/// captures in this tree — so it is read off the bundle the persona was loaded from, exactly as the
/// surface is, and swapping the bundle swaps it with everything else the bundle says.
///
/// **This is a PERSONA answer, not a capability.** Ruling R7 still stands: the engine has no demuxer
/// and no decoder, `readyState` never leaves `HAVE_NOTHING` and `play()` rejects. `canPlayType` has
/// always been the recorded browser's answer; making it follow the worn device makes the CLAIM match
/// the device, and moves nothing toward playback.
#[derive(Debug, Default)]
pub struct CodecGrid {
    /// The capture id this came from — the key its derived query table is memoised under.
    pub capture_id: String,
    /// Container mime (lower-cased as the capture asked it) -> that container's answers.
    pub containers: std::collections::BTreeMap<String, CodecContainer>,
    /// Every codec spelling the sweep asked `canPlayType` about, accepted or not, sorted and distinct. A codec
    /// the device was ASKED about and accepted nowhere is one it does not decode; one it was never asked about
    /// is not a fact the capture holds, and the grammar answers it as Chromium's own build does (PHASE 3 F7).
    pub asked: Vec<String>,
}

/// An 8-bit RGBA pixel buffer: row-major, top-left origin, unpremultiplied — the layout `getImageData`
/// and `readPixels` expose. `data.len() == width*height*4`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Rgba8 {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Rgba8 {
    /// A fully-transparent buffer of the given size.
    pub fn transparent(width: u32, height: u32) -> Self {
        Self { width, height, data: vec![0; (width as usize) * (height as usize) * 4] }
    }
}

/// A video track's format, as [`RenderBackend::video_decoder`] is asked for it: the ISO-BMFF sample entry's four-cc
/// (`avc1`/`avc3` H.264, `av01` AV1, `vp09` VP9, `vp08` VP8) and its configuration record (`avcC`, `av1C`, `vpcC`) --
/// a WebM track's `CodecPrivate` is the same record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoCodec {
    pub fourcc: [u8; 4],
    pub config: Vec<u8>,
}

/// One decoded video frame: 8-bit planar 4:2:0 YUV (luma `width * height`, each chroma plane
/// `((width + 1) / 2) * ((height + 1) / 2)`), with the colour description the stream carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YuvFrame {
    pub width: u32,
    pub height: u32,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    /// ISO/IEC 23091-2 matrix coefficients: 1 BT.709, 5 and 6 BT.601, 9 BT.2020, 2 unspecified.
    pub matrix: u8,
    pub full_range: bool,
}

impl YuvFrame {
    /// The frame as unpremultiplied RGBA8, chroma interpolated between its samples. An unspecified matrix is BT.709, as
    /// Chrome takes one from a `MediaSource` (WPT's `test-v-128k-320x240` red reads `255, 24, 0` in Chrome 150); a file's
    /// player names BT.601 for its SD frames before this is asked.
    pub fn to_rgba(&self) -> Rgba8 {
        let (w, h) = (self.width as usize, self.height as usize);
        let cw = w.div_ceil(2);
        let ch = h.div_ceil(2);
        let matrix = match self.matrix {
            2 => 1,
            m => m,
        };
        let (kr, kb) = match matrix {
            5 | 6 => (0.299, 0.114),
            9 | 10 => (0.2627, 0.0593),
            _ => (0.2126, 0.0722),
        };
        let kg = 1.0 - kr - kb;
        // fixed point, 16 fractional bits: R = Y + rv*V, G = Y - gu*U - gv*V, B = Y + bu*U
        let (ys, cs) = if self.full_range { (1.0, 1.0) } else { (255.0 / 219.0, 255.0 / 224.0) };
        let fx = |v: f64| (v * 65536.0).round() as i32;
        let yk = fx(ys);
        let rv = fx(2.0 * (1.0 - kr) * cs);
        let bu = fx(2.0 * (1.0 - kb) * cs);
        let gu = fx(2.0 * (1.0 - kb) * kb / kg * cs);
        let gv = fx(2.0 * (1.0 - kr) * kr / kg * cs);
        let y0 = if self.full_range { 0 } else { 16 };
        let mut data = vec![255u8; w * h * 4];
        if self.y.len() < w * h || self.u.len() < cw * ch || self.v.len() < cw * ch {
            return Rgba8 { width: self.width, height: self.height, data };
        }
        let clamp = |v: i32| ((v + 32768) >> 16).clamp(0, 255) as u8;
        // chroma at each pixel: the two nearest samples on each axis, weighted 3:1 (the samples sit between luma
        // pairs) -- a row's vertical blend first, sixteenths kept, then each pixel's horizontal one
        let (mut ur, mut vr) = (vec![0i32; cw], vec![0i32; cw]);
        for y in 0..h {
            let cy = y / 2;
            let ny = if y % 2 == 0 { cy.saturating_sub(1) } else { (cy + 1).min(ch - 1) };
            let (a, b) = (cy * cw, ny * cw);
            for cx in 0..cw {
                ur[cx] = 3 * self.u[a + cx] as i32 + self.u[b + cx] as i32;
                vr[cx] = 3 * self.v[a + cx] as i32 + self.v[b + cx] as i32;
            }
            let row = &self.y[y * w..y * w + w];
            let out = &mut data[y * w * 4..(y + 1) * w * 4];
            for x in 0..w {
                let cx = x / 2;
                let nx = if x % 2 == 0 { cx.saturating_sub(1) } else { (cx + 1).min(cw - 1) };
                let u = ((3 * ur[cx] + ur[nx] + 8) >> 4) - 128;
                let v = ((3 * vr[cx] + vr[nx] + 8) >> 4) - 128;
                let px = &mut out[x * 4..x * 4 + 3];
                if matrix == 0 {
                    // identity (GBR): Y is G, U is B, V is R
                    px[0] = (v + 128) as u8;
                    px[1] = row[x];
                    px[2] = (u + 128) as u8;
                    continue;
                }
                let l = (row[x] as i32 - y0) * yk;
                px[0] = clamp(l + rv * v);
                px[1] = clamp(l - gu * u - gv * v);
                px[2] = clamp(l + bu * u);
            }
        }
        Rgba8 { width: self.width, height: self.height, data }
    }
}

/// One video track's decoder ([`RenderBackend::video_decoder`]), on a media thread of its own. Samples go in in
/// DECODE order and frames come out in PRESENTATION order, as soon as the decoder has them: a stream with B-frames
/// holds some back until a later sample settles their order.
pub trait VideoDecoder: Send {
    /// One sample as the container frames it (length-prefixed NAL units for H.264, a temporal unit's OBUs for AV1):
    /// the frames it completes.
    fn decode(&mut self, sample: &[u8]) -> Vec<YuvFrame>;
    /// The frames still held back -- the samples ended, or the next one is a seek's keyframe.
    fn flush(&mut self) -> Vec<YuvFrame>;
}

/// A paint colour as Skia receives one from Blink -- `Color::toSkColor4f()`: UNPREMULTIPLIED, EXTENDED sRGB (a
/// display-p3 red is `[1.093, -0.227, -0.150, 1]`), alpha last. Skia takes it into the canvas's own colour space
/// and clamps when it stores (the colour was 8-bit sRGB here, which has no room for
/// `color(display-p3 ...)`).
pub type Color4f = [f32; 4];

/// A legacy colour -- 8-bit channels, the alpha CSS parsed -- as Blink paints it: `rgb()` keeps its channels as
/// floats in [0, 255] and `toSkColor4f` divides each by 255 in double (`SRGBLegacyToSRGB`).
pub fn legacy_color4(r: u8, g: u8, b: u8, alpha: f32) -> Color4f {
    [(r as f64 / 255.0) as f32, (g as f64 / 255.0) as f32, (b as f64 / 255.0) as f32, alpha]
}

/// One recorded Canvas 2D drawing operation. The `webapi` front-end appends these as the page draws;
/// the active [`RenderBackend`] rasterizes them at readback. R0 defines only enough to prove the
/// record→raster→readback pipeline (fill-style + rectangles); R2 grows it (paths, text, gradients,
/// compositing modes, transforms). CSS-color parsing stays above the kernel, so colours arrive as
/// [`Color4f`] paint colours.
#[derive(Debug, Clone, PartialEq)]
pub enum Canvas2DOp {
    /// Set the current fill color.
    SetFillStyle { color: Color4f },
    /// `fillRect(x, y, w, h)` with the current fill style.
    FillRect { x: f64, y: f64, w: f64, h: f64 },
    /// `clearRect(x, y, w, h)` — set the region to transparent black.
    ClearRect { x: f64, y: f64, w: f64, h: f64 },
    // --- R2.b.2 path building (the current path; `Fill` paints it with the current fill style) ---
    /// `beginPath()` — start a fresh path.
    BeginPath,
    /// `moveTo(x, y)` — start a new sub-path at a point.
    MoveTo { x: f64, y: f64 },
    /// `lineTo(x, y)` — add a line segment to the current sub-path.
    LineTo { x: f64, y: f64 },
    /// `arc(x, y, radius, startAngle, endAngle, counterclockwise)`.
    Arc { x: f64, y: f64, radius: f64, start: f64, end: f64, ccw: bool },
    /// `rect(x, y, w, h)` — add a closed rectangle sub-path (not a fill).
    Rect { x: f64, y: f64, w: f64, h: f64 },
    /// `closePath()` — close the current sub-path.
    ClosePath,
    /// `fill([rule])` — paint the current path with the current fill style. `even_odd` = the "evenodd"
    /// fill rule (default is nonzero/winding).
    Fill { even_odd: bool },
    // --- R2.b.3 text (the biggest canvas fingerprint signal — resolved through the OS font stack) ---
    /// The current font, parsed from the CSS `font` shorthand (size already in CSS px).
    ///
    /// `families` is the whole `font-family` list, `lang` the language the text is shaped in (the UI
    /// locale) and `default_family` the face a list naming nothing installed falls back to -- what
    /// [`RenderBackend::text_ink`] resolves text with, so `fillText` draws the glyphs `measureText`
    /// measures. Empty = the first `family` alone, no language. `weight` is the CSS `font-weight` (1 to 1000), which
    /// picks among a web font's faces; `bold` is that weight at 600 or more.
    SetFont { family: String, size_px: f64, bold: bool, italic: bool, weight: u16, families: String, lang: String, default_family: String },
    /// `fillText(text, x, y [, maxWidth])` — draw text at the baseline origin with the current font +
    /// fill style. `max_width` condenses the run horizontally when the natural advance exceeds it.
    FillText { text: String, x: f64, y: f64, max_width: Option<f64> },
    /// `globalCompositeOperation` — how subsequent draws blend with the existing pixels.
    SetGlobalCompositeOperation { op: CompositeOp },

    // ── 3.1 the rest of the 2D surface. Every variant below is EXECUTED by the Skia backend; a
    // recorded op the backend drops would let `getImageData` and `toDataURL` claim a fidelity the
    // pixels do not have, which is the one thing a display list must never do.

    // --- drawing state stack ---
    /// `save()` — push the whole drawing state (transform, clip, paint, text, shadow, dash).
    Save,
    /// `restore()` — pop it. A `restore` with an empty stack is a no-op, per HTML.
    Restore,
    /// Push the drawing state as `Save` does and draw what follows into a layer of its own, composited at the
    /// matching `Restore` with `alpha` -- a CSS `opacity` group, which fades its whole subtree as one picture (where
    /// overlapping children would show through each other with a per-draw `globalAlpha`). The document paint's,
    /// not a canvas API's (no 2D context method records it).
    SaveLayer { alpha: f64 },
    /// A layer whose content is drawn through `filters`, in order, when the matching `Restore` ends it: CSS `filter`
    /// on an element and everything it paints.
    SaveLayerFilter { filters: Vec<CanvasFilter> },
    /// A layer that starts as what is already drawn inside `rect` (the current coordinates) through `filters` --
    /// CSS `backdrop-filter` behind an element, whose border box the caller clips to first. Restored at once, it
    /// leaves that part of the backdrop filtered.
    SaveLayerBackdrop { filters: Vec<CanvasFilter>, rect: [f64; 4] },
    /// A layer composited onto what is beneath it with `op` when the matching `Restore` ends it: CSS `mix-blend-mode`
    /// on an element and everything it paints.
    SaveLayerBlend { op: CompositeOp },

    // --- transforms. The recorder keeps the same matrix so `getTransform()` can answer without the
    // backend; these carry the change so the backend reproduces it in the same order. ---
    /// `transform(a,b,c,d,e,f)` / `translate` / `rotate` / `scale` — multiply into the current matrix.
    Transform { t: Transform2D },
    /// `setTransform(...)` / `resetTransform()` — replace the current matrix outright.
    SetTransform { t: Transform2D },

    // --- paint state ---
    /// Set the current stroke color.
    SetStrokeStyle { color: Color4f },
    /// `fillStyle = <gradient>` — paint fills with the gradient defined under `id`.
    SetFillGradient { id: u32 },
    /// `strokeStyle = <gradient>`.
    SetStrokeGradient { id: u32 },
    /// `fillStyle = <pattern>`.
    SetFillPattern { id: u32 },
    /// `strokeStyle = <pattern>`.
    SetStrokePattern { id: u32 },
    /// `globalAlpha` — 0..1, multiplied into every subsequent draw's alpha.
    SetGlobalAlpha { alpha: f64 },
    /// `lineWidth`, in user-space units (so the current transform scales it).
    SetLineWidth { width: f64 },
    /// `lineCap`.
    SetLineCap { cap: LineCap },
    /// `lineJoin`.
    SetLineJoin { join: LineJoin },
    /// `miterLimit`.
    SetMiterLimit { limit: f64 },
    /// `setLineDash([...])` — an empty list is a solid line.
    SetLineDash { segments: Vec<f64> },
    /// `lineDashOffset`.
    SetLineDashOffset { offset: f64 },
    /// `shadowColor` — a fully transparent shadow color disables the shadow, per HTML.
    SetShadowColor { color: Color4f },
    /// `shadowBlur` — the HTML blur *diameter*; the backend halves it for a Gaussian sigma.
    SetShadowBlur { blur: f64 },
    /// `shadowOffsetX` / `shadowOffsetY`, in user-space units.
    SetShadowOffset { x: f64, y: f64 },
    /// `filter` — the parsed filter chain, empty for `none`.
    SetFilter { filters: Vec<CanvasFilter> },
    /// `imageSmoothingEnabled`.
    SetImageSmoothing { enabled: bool },
    /// `imageSmoothingQuality`.
    SetImageSmoothingQuality { quality: SmoothingQuality },

    // --- text state ---
    /// `textAlign`.
    SetTextAlign { align: TextAlign },
    /// `textBaseline`.
    SetTextBaseline { baseline: TextBaseline },
    /// `direction` — the bidi base direction `start`/`end` alignment resolves against.
    SetTextDirection { dir: TextDirection },
    /// `letterSpacing`, in CSS px (0 = the font's own advances, untouched).
    SetLetterSpacing { px: f64 },
    /// `wordSpacing`, in CSS px.
    SetWordSpacing { px: f64 },

    // --- path building (the current path; the ops above stay for the shapes R2 already had) ---
    /// `bezierCurveTo(cp1x, cp1y, cp2x, cp2y, x, y)`.
    BezierCurveTo { cp1x: f64, cp1y: f64, cp2x: f64, cp2y: f64, x: f64, y: f64 },
    /// `quadraticCurveTo(cpx, cpy, x, y)`.
    QuadraticCurveTo { cpx: f64, cpy: f64, x: f64, y: f64 },
    /// `arcTo(x1, y1, x2, y2, radius)` — the tangent arc, which needs the current point.
    ArcTo { x1: f64, y1: f64, x2: f64, y2: f64, radius: f64 },
    /// `ellipse(x, y, rx, ry, rotation, start, end, ccw)`.
    Ellipse { x: f64, y: f64, rx: f64, ry: f64, rotation: f64, start: f64, end: f64, ccw: bool },
    /// `roundRect(x, y, w, h, radii)` — four already-normalised (rx, ry) corner radii,
    /// in top-left, top-right, bottom-right, bottom-left order.
    RoundRect { x: f64, y: f64, w: f64, h: f64, radii: [(f64, f64); 4] },

    // --- painting ---
    /// `stroke()` — stroke the current path with the current stroke style + line state.
    Stroke,
    /// `strokeRect(x, y, w, h)` — strokes without touching the current path.
    StrokeRect { x: f64, y: f64, w: f64, h: f64 },
    /// `strokeText(text, x, y [, maxWidth])`.
    StrokeText { text: String, x: f64, y: f64, max_width: Option<f64> },
    /// `clip([rule])` — intersect the clip region with the current path.
    Clip { even_odd: bool },

    // --- an explicit path (a `Path2D`), passed to fill/stroke/clip. Carried BY VALUE, because a
    // `Path2D` mutated after the call must not change a draw that already happened. ---
    /// `fill(path [, rule])`.
    FillPath { path: Vec<PathOp>, even_odd: bool },
    /// `stroke(path)`.
    StrokePath { path: Vec<PathOp> },
    /// `clip(path [, rule])`.
    ClipPath { path: Vec<PathOp>, even_odd: bool },

    // --- gradients and patterns. Defined as ops rather than a side table so that a stop added
    // AFTER a draw affects only the draws that follow it, which is what a live `CanvasGradient`
    // object does in a browser. ---
    /// `createLinearGradient(x0, y0, x1, y1)`.
    DefineLinearGradient { id: u32, x0: f64, y0: f64, x1: f64, y1: f64 },
    /// `createRadialGradient(x0, y0, r0, x1, y1, r1)`.
    DefineRadialGradient { id: u32, x0: f64, y0: f64, r0: f64, x1: f64, y1: f64, r1: f64 },
    /// `createConicGradient(startAngle, x, y)`.
    DefineConicGradient { id: u32, start_angle: f64, x: f64, y: f64 },
    /// `gradient.addColorStop(offset, color)`. Blink interpolates a gradient in sRGB when every stop is `legacy`
    /// (an `rgb()`/`hsl()`/`hwb()`/hex/named colour with no `none`), else in OKLab with each stop's `oklab` colour
    /// (converted to OKLab and back) -- see [`CanvasColor`].
    AddColorStop { id: u32, offset: f64, color: Color4f, oklab: Color4f, legacy: bool },
    /// Gradient `id` interpolates with premultiplied alpha, as a CSS gradient does (`CSSGradientValue` builds its
    /// `Gradient` with `ColorInterpolation::kPremultiplied`; a `CanvasGradient` is `kUnpremultiplied`): a stop
    /// fading to `transparent` keeps its hue instead of passing through grey.
    PremultiplyGradient { id: u32 },
    /// `createPattern(image, repetition)` — the source pixels, snapshotted at creation.
    DefinePattern { id: u32, image: Rgba8, repeat: PatternRepeat },
    /// `pattern.setTransform(matrix)`.
    SetPatternTransform { id: u32, t: Transform2D },
    /// A pattern whose tile is `ops` drawn into a `w` x `h` tile (its origin the tile's top-left), at `w.ceil()` x
    /// `h.ceil()` pixels -- what a `SetPatternTransform` scales back -- repeated as `repeat` says: a CSS gradient layer
    /// sized by `background-size` and repeated by `background-repeat`. The document paint's, not a canvas API's.
    DefineTilePattern { id: u32, w: f64, h: f64, ops: Vec<Canvas2DOp>, repeat: PatternRepeat },

    // --- images ---
    /// `drawImage(...)` in its three-argument, five-argument and nine-argument forms, already
    /// normalised to a source rect and a destination rect.
    DrawImage { image: Rgba8, sx: f64, sy: f64, sw: f64, sh: f64, dx: f64, dy: f64, dw: f64, dh: f64 },
    /// `putImageData(data, dx, dy [, dirtyX, dirtyY, dirtyWidth, dirtyHeight])`. Ignores the
    /// transform, the clip, globalAlpha and the composite mode — it writes pixels, per HTML.
    PutImageData { image: Rgba8, dx: f64, dy: f64, dirty_x: f64, dirty_y: f64, dirty_w: f64, dirty_h: f64 },
}

/// What [`RenderBackend::hit_test_2d`] is being asked about a point.
#[derive(Debug, Clone, PartialEq)]
pub enum HitTest {
    /// `isPointInPath(x, y [, rule])` — is the point inside the current path?
    InPath { even_odd: bool },
    /// `isPointInStroke(x, y)` — is it inside the stroked outline of the current path?
    InStroke,
    /// `isPointInPath(path, x, y [, rule])` — the same question about an explicit `Path2D`.
    InExplicitPath { path: Vec<PathOp>, even_odd: bool },
    /// `isPointInStroke(path, x, y)`.
    InExplicitStroke { path: Vec<PathOp> },
}

/// The canvas `globalCompositeOperation` modes (Porter-Duff + separable/non-separable blends). Named at
/// the spec layer (no `skia_safe` dependency here); the render backend maps these to its `BlendMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompositeOp {
    SourceOver,
    SourceIn,
    SourceOut,
    SourceAtop,
    DestinationOver,
    DestinationIn,
    DestinationOut,
    DestinationAtop,
    Lighter,
    Copy,
    Xor,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

/// A 2x3 affine transform, in the order `DOMMatrix` names its members: `[a c e]` over `[b d f]`.
/// The canvas's current transformation matrix is one of these, and so is a pattern's. Plain data (no
/// `skia_safe`), like everything else in this crate; the backend maps it to whatever it uses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform2D {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Default for Transform2D {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Transform2D {
    pub const IDENTITY: Self = Self { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 };

    pub fn translate(tx: f64, ty: f64) -> Self {
        Self { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: tx, f: ty }
    }
    pub fn scale(sx: f64, sy: f64) -> Self {
        Self { a: sx, b: 0.0, c: 0.0, d: sy, e: 0.0, f: 0.0 }
    }
    pub fn rotate(radians: f64) -> Self {
        let (s, c) = radians.sin_cos();
        Self { a: c, b: s, c: -s, d: c, e: 0.0, f: 0.0 }
    }

    /// `self ∘ inner` — the matrix a canvas ends up with after `ctx.transform(inner)` while its
    /// current matrix is `self`. Points map through `inner` first, which is what makes two
    /// `translate()` calls accumulate rather than replace each other.
    pub fn multiply(&self, inner: &Transform2D) -> Transform2D {
        Transform2D {
            a: self.a * inner.a + self.c * inner.b,
            b: self.b * inner.a + self.d * inner.b,
            c: self.a * inner.c + self.c * inner.d,
            d: self.b * inner.c + self.d * inner.d,
            e: self.a * inner.e + self.c * inner.f + self.e,
            f: self.b * inner.e + self.d * inner.f + self.f,
        }
    }

    pub fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }

    pub fn is_finite(&self) -> bool {
        [self.a, self.b, self.c, self.d, self.e, self.f].iter().all(|v| v.is_finite())
    }

    pub fn determinant(&self) -> f64 {
        self.a * self.d - self.b * self.c
    }

    /// The inverse, or `None` when the matrix is singular — the case HTML makes a silent no-op
    /// rather than an error.
    pub fn invert(&self) -> Option<Transform2D> {
        let det = self.determinant();
        if det == 0.0 || !det.is_finite() {
            return None;
        }
        let inv = 1.0 / det;
        Some(Transform2D {
            a: self.d * inv,
            b: -self.b * inv,
            c: -self.c * inv,
            d: self.a * inv,
            e: (self.c * self.f - self.d * self.e) * inv,
            f: (self.b * self.e - self.a * self.f) * inv,
        })
    }

    /// Map a point through this matrix.
    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        (self.a * x + self.c * y + self.e, self.b * x + self.d * y + self.f)
    }
}

/// `lineCap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineCap {
    #[default]
    Butt,
    Round,
    Square,
}

/// `lineJoin`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineJoin {
    #[default]
    Miter,
    Round,
    Bevel,
}

/// `textAlign`. `Start`/`End` resolve against `direction` at paint time, which is why they stay
/// distinct from `Left`/`Right` here instead of being folded in by the recorder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextAlign {
    #[default]
    Start,
    End,
    Left,
    Right,
    Center,
}

/// `textBaseline`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextBaseline {
    Top,
    Hanging,
    Middle,
    #[default]
    Alphabetic,
    Ideographic,
    Bottom,
}

/// `direction` — the canvas's bidi base direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextDirection {
    Ltr,
    Rtl,
    #[default]
    Inherit,
}

/// `imageSmoothingQuality`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SmoothingQuality {
    #[default]
    Low,
    Medium,
    High,
}

/// `createPattern`'s repetition argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PatternRepeat {
    #[default]
    Repeat,
    RepeatX,
    RepeatY,
    NoRepeat,
}

/// One function of the `filter` shorthand, already parsed (lengths in CSS px, ratios as plain
/// numbers rather than percentages). Named at the spec layer the way [`CompositeOp`] is, and every
/// variant is one the backend executes: a filter function the backend dropped would make
/// `ctx.filter` a value that reads back correctly and paints nothing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CanvasFilter {
    Blur { sigma: f64 },
    Brightness { amount: f64 },
    Contrast { amount: f64 },
    Grayscale { amount: f64 },
    HueRotate { degrees: f64 },
    Invert { amount: f64 },
    Opacity { amount: f64 },
    Saturate { amount: f64 },
    Sepia { amount: f64 },
    DropShadow { dx: f64, dy: f64, sigma: f64, r: u8, g: u8, b: u8, a: u8 },
}

/// One segment of a path built independently of a context's current path — what a `Path2D` holds.
/// Coordinates are in the space the path was built in; the backend maps them through whatever
/// transform applies where the path is used.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PathOp {
    MoveTo { x: f64, y: f64 },
    LineTo { x: f64, y: f64 },
    BezierCurveTo { cp1x: f64, cp1y: f64, cp2x: f64, cp2y: f64, x: f64, y: f64 },
    QuadraticCurveTo { cpx: f64, cpy: f64, x: f64, y: f64 },
    Arc { x: f64, y: f64, radius: f64, start: f64, end: f64, ccw: bool },
    ArcTo { x1: f64, y1: f64, x2: f64, y2: f64, radius: f64 },
    Ellipse { x: f64, y: f64, rx: f64, ry: f64, rotation: f64, start: f64, end: f64, ccw: bool },
    Rect { x: f64, y: f64, w: f64, h: f64 },
    RoundRect { x: f64, y: f64, w: f64, h: f64, radii: [(f64, f64); 4] },
    ClosePath,
}

/// The device-varying inputs to the render engine — the "extract once, parameterize" seam.
/// Every value the engine consumes that differs by device
/// flows through here; the engine holds NO hardcoded local constants, so generalizing to other devices is
/// "collect params, plug in", never "re-derive a renderer". R0 defines the skeleton; R2/R3/R4 add
/// fonts, color profile, GPU/ANGLE identity, and the farbling policy. It is filled from the local
/// machine (autonomous) + minted seeds, or from a collected host-GPU-family device.
#[derive(Debug, Clone, Default)]
pub struct RenderParams {
    /// `window.devicePixelRatio` — the canvas backing-store scale.
    pub device_pixel_ratio: f64,
    /// A 2D canvas's colour space, `getContext('2d', {colorSpace})` -- the space its backing holds and Skia takes
    /// paint colours into.
    pub color_space: CanvasColorSpace,
    /// `getContext('2d', {colorType: 'float16'})`: the backing holds half floats rather than 8-bit channels.
    pub float16: bool,
    /// `getContext('2d', {alpha: false})`: an OPAQUE backing (Blink's `kOpaque_SkAlphaType`), opaque black until
    /// drawn on, whose text may be LCD (stock Chrome 150 reads `rgba(255,0,0,0.5)` filled
    /// on one as `[128, 0, 0, 255]`).
    pub opaque: bool,
    /// The top-level document's frame, not a canvas: Chrome's root layer, opaque on its white base background, whose
    /// text is LCD-antialiased (`ComputeSurfaceProps` with LCD text allowed; text inside an effect's layer is not).
    pub document: bool,
}

/// The colour spaces a canvas or an `ImageData` can be in -- `PredefinedColorSpace` as Chrome 150 enables it
/// (Blink's `PredefinedColorSpaceToSkColorSpace`: sRGB, and display-p3 = the sRGB transfer function over
/// `SkNamedGamut::kDisplayP3`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CanvasColorSpace {
    #[default]
    Srgb,
    DisplayP3,
}

/// The pixel format an `ImageData` is read back in: `rgba-unorm8` bytes, or `rgba-float16` / `rgba-float32`
/// little-endian floats -- four channels a pixel, unpremultiplied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadFormat {
    Unorm8,
    Float16,
    Float32,
}

/// The pixel engine behind canvas/WebGL/audio. The `render` crate implements it (`NullBackend`, the
/// trivial R0 `SolidBackend`, and — from R2 — Skia); `webapi` records ops and calls back here for
/// readback. A live rasterizer, NOT the legacy static replay oracle.
/// How a printed document is cut into pages ([`RenderBackend::print_pdf`]).
#[derive(Debug, Clone, PartialEq)]
pub struct PdfLayout {
    /// The paper, in points (1/72 in), landscape already applied.
    pub paper_width: f32,
    pub paper_height: f32,
    /// The margins, in points: top, right, bottom, left.
    pub margins: [f32; 4],
    /// Points per CSS px: 72/96 times the print scale.
    pub points_per_px: f32,
    /// One page's content height, in CSS px.
    pub page_height_px: f32,
    /// The pages to draw, 0-based, in order.
    pub pages: Vec<u32>,
    /// The document's title, for the PDF's metadata.
    pub title: String,
}

pub trait RenderBackend: Send + Sync {
    /// Human-readable backend name (e.g. "null", "solid", "skia").
    fn name(&self) -> &str;

    /// Rasterize a recorded 2D display list into an RGBA8 buffer of the canvas's backing size. The
    /// default is transparent (the null backend); real backends execute the ops. `getImageData` /
    /// `toDataURL` read from the returned buffer.
    fn raster_2d(&self, _ops: &[Canvas2DOp], width: u32, height: u32, _params: &RenderParams) -> Rgba8 {
        Rgba8::transparent(width, height)
    }

    /// Rasterize the display list but return ONLY the sub-region `(rx, ry, rw, rh)` of
    /// the full `width`×`height` canvas — an `rw`×`rh` buffer whose pixels are exactly what the full
    /// raster would produce there. The ops run in full, so state (fill style, font, gradients) is
    /// correct; only the region is produced. For the tile-damage compositor: re-raster only the tiles a
    /// frame changed and composite them over the cached frame. The default rasters the whole frame and
    /// crops (correct, not faster) — a real backend (Skia) overrides to draw only the region.
    fn raster_2d_region(&self, ops: &[Canvas2DOp], width: u32, height: u32, rx: u32, ry: u32, rw: u32, rh: u32, params: &RenderParams) -> Rgba8 {
        let full = self.raster_2d(ops, width, height, params);
        let mut data = vec![0u8; (rw as usize) * (rh as usize) * 4];
        let full_row = (width as usize) * 4;
        let out_row = (rw as usize) * 4;
        for row in 0..(rh as usize) {
            let sy = ry as usize + row;
            if sy >= height as usize {
                break;
            }
            let src = sy * full_row + (rx as usize) * 4;
            let copy = out_row.min(full_row.saturating_sub((rx as usize) * 4));
            if src + copy <= full.data.len() {
                data[row * out_row..row * out_row + copy].copy_from_slice(&full.data[src..src + copy]);
            }
        }
        Rgba8 { width: rw, height: rh, data }
    }

    /// `getImageData(sx, sy, sw, sh, {colorSpace, pixelFormat})` as Blink reads it: the
    /// region of the canvas's backing ([`RenderParams`] says its colour space and type) converted by Skia into
    /// `space` and `format`, unpremultiplied -- `snapshot.readPixels(ImageData::GetSkPixmap().info(), sx, sy)`.
    /// Pixels outside the canvas are transparent black. `None` = this backend cannot convert; the caller then
    /// reads [`RenderBackend::raster_2d_region`]'s bytes as they are.
    #[allow(clippy::too_many_arguments)]
    fn read_2d(&self, _ops: &[Canvas2DOp], _width: u32, _height: u32, _sx: i32, _sy: i32, _sw: u32, _sh: u32, _params: &RenderParams, _space: CanvasColorSpace, _format: ReadFormat) -> Option<Vec<u8>> {
        None
    }

    /// `isPointInPath` / `isPointInStroke` — is the canvas-space point `(x, y)` inside the path the
    /// display list has built by its end?
    ///
    /// This belongs to the BACKEND rather than to the recorder for the same reason `raster_2d` does.
    /// The answer is a geometry question about arcs, Béziers, winding rules and stroke outlines, and
    /// a second implementation of that geometry in `webapi` would be a second implementation that
    /// disagrees: a page can ask `isPointInPath` and then fill the same path and read the pixel back,
    /// and the two answers must come from the same curve flattener. `false` is the null backend's
    /// answer — nothing has a path.
    fn hit_test_2d(&self, _ops: &[Canvas2DOp], _x: f64, _y: f64, _what: &HitTest, _params: &RenderParams) -> bool {
        false
    }

    /// Encode a rastered buffer as image bytes — what `canvas.toDataURL(type, quality)` serializes.
    ///
    /// This belongs to the BACKEND, not to the caller, for the same reason `raster_2d` does: the bytes
    /// are the fingerprint. A page hashes the base64 string, so the encoder's own choices — filter
    /// selection, zlib level, chroma subsampling — are part of what it is measuring, and an encoder
    /// picked for convenience would answer a question about the encoder rather than about the pixels.
    /// Routing it here keeps it with the Skia the rasterizer already uses, which is the same library
    /// Chrome encodes with.
    ///
    /// `None` = this backend cannot produce that format; the caller falls back to PNG, which is what
    /// the HTML spec requires of an unsupported `type` argument anyway.
    fn encode_image(&self, _buf: &Rgba8, _format: ImageFormat, _quality: Option<f64>) -> Option<Vec<u8>> {
        None
    }

    /// A printed document (`Page.printToPDF`): the page's display list -- laid out under `print` media at the
    /// paper's content width, `width` x `height` CSS px -- cut into the pages `layout` describes and drawn into a PDF,
    /// its text as text. `None` = this backend makes no PDF.
    fn print_pdf(&self, _ops: &[Canvas2DOp], _width: u32, _height: u32, _layout: &PdfLayout) -> Option<Vec<u8>> {
        None
    }

    /// [`RenderBackend::encode_image`] for a snapshot that may carry a colour space. `srgb` = the snapshot is a
    /// 2D context's resource-provider image, which Blink tags sRGB, and Skia's JPEG and WebP encoders then embed
    /// that profile (an APP2 / ICCP chunk); Blink's transparent image of a canvas never drawn on has none (Chrome
    /// 150, measured: the same 8x8 canvas exports 1035 or 403 characters of JPEG by that alone). The default
    /// ignores the tag.
    fn encode_image_tagged(&self, buf: &Rgba8, format: ImageFormat, quality: Option<f64>, srgb: bool) -> Option<Vec<u8>> {
        let _ = srgb;
        self.encode_image(buf, format, quality)
    }

    /// The part of a canvas `measureText` that needs the FONT FILES: the text
    /// shaped as Chrome shapes it, each glyph's ink box at its pen position, and the primary font's
    /// `head`/`OS/2`/`BASE` values Blink's `TextMetrics` reads. It belongs to the backend because the
    /// glyph outlines and the rasterizer's hinting are the backend's -- the same that `fillText` draws
    /// with, so the measured box and the drawn pixels come from one font. `None` = no fonts here.
    fn text_ink(&self, _font: &TextFontRequest, _text: &str) -> Option<TextInk> {
        None
    }

    /// The installed font a CSS `local()` name matches (CSS Fonts 4 `src`), as Chrome on Linux finds it:
    /// Chromium's `FontConfigLocalMatching` asks fontconfig for a scalable font whose PostScript name,
    /// else full name, is `name` (case does not matter), and takes the first one in an sfnt file that exists. A
    /// `FontFace` whose source is `local("DejaVu Sans")` loads by it -- a bot-challenge frame probes 55 names
    /// that way. `None` = no installed font has that name (or no fonts here).
    fn local_font(&self, _name: &str) -> Option<LocalFont> {
        None
    }

    /// A font the page downloaded (an `@font-face` source: sfnt, WOFF or WOFF2), registered for drawing: the family
    /// name a display list names it by, which no installed font answers to. The document's painter asks for each face
    /// it draws in, every frame, so a font already registered under `url` answers without decoding `body` again.
    /// `None` = not a font (or no fonts here).
    fn web_font(&self, _url: &str, _body: &[u8]) -> Option<String> {
        None
    }

    /// The metrics layout measures text in a registered web font with ([`RenderBackend::web_font`]'s family name),
    /// read off its file as Chrome's font stack reads them. `None` = not a registered font (or no fonts here).
    fn web_font_metrics(&self, _family: &str) -> Option<FontFileMetrics> {
        None
    }

    /// Decode an encoded still image (PNG, JPEG, GIF's first frame, BMP, ICO, ...) into UNPREMULTIPLIED
    /// RGBA8 — the pixels `ctx.drawImage(img, ...)` paints and `createImageBitmap(blob)` holds.
    ///
    /// The encoder's reasons apply to the decoder: the pixels ARE the fingerprint (a JPEG decoded by a
    /// different IDCT reads back different bytes from `getImageData`), so it belongs with the Skia the
    /// rasterizer and the encoder already use, which is also Chrome's image decoding library. `None` =
    /// not an image this backend can decode, which a caller treats as a decode failure — never as an
    /// empty image.
    fn decode_image(&self, _bytes: &[u8]) -> Option<Rgba8> {
        None
    }

    /// The size an encoded image's HEADER gives, or `None` when it does not parse. Blink
    /// knows an image's size once the header parses: such an `<img>` fires `load` and reports its natural size
    /// even when its pixel data then fails to decode ([`RenderBackend::decode_image`] `None`), and draws nothing
    /// at all then. The default reads the size off a full decode.
    fn image_size(&self, bytes: &[u8]) -> Option<(u32, u32)> {
        self.decode_image(bytes).map(|i| (i.width, i.height))
    }

    /// The first frame as Blink holds it once all the data has arrived: its pixels as decoded so far (rows not
    /// decoded are transparent black) and whether the decode COMPLETED; `None` when the header does not parse.
    /// `drawImage` draws what there is -- stock Chrome 150 draws a truncated PNG's decoded
    /// rows -- and `createImageBitmap` takes only a complete frame. The default is [`RenderBackend::decode_image`]:
    /// complete or nothing.
    fn decode_frame(&self, bytes: &[u8]) -> Option<(Rgba8, bool)> {
        self.decode_image(bytes).map(|i| (i, true))
    }

    /// A decoder for one video track ([`VideoCodec`]): what a `<video>` shows. `None` = no decoder for that format
    /// here, and the element shows its poster, as a browser built without the codec would.
    fn video_decoder(&self, _codec: &VideoCodec) -> Option<Box<dyn VideoDecoder>> {
        None
    }

    /// Execute a recorded GL command list and return the drawing buffer as RGBA8 — the WebGL half of
    /// `raster_2d`, and the single source both `readPixels` and `toDataURL` read.
    ///
    /// "Single source" is the whole point. The 2D path once had `getImageData` rastering live while
    /// `toDataURL` replayed a recording, and the disagreement between them was a one-expression tell.
    /// Here they are the same buffer by construction: `readPixels` copies a region out of what this
    /// returns (flipping y, because GL's origin is bottom-left and [`Rgba8`]'s is top-left), and
    /// `toDataURL` hands the whole thing to [`RenderBackend::encode_image`].
    ///
    /// Defaulted to transparent so `NullBackend`/`SolidBackend` keep compiling — but a backend that
    /// takes the default is a backend where WebGL draws nothing, which is exactly the state R3 exists
    /// to end.
    ///
    /// `list` names the op list `cmds` is, as [`RenderBackend::query_gl`]'s does: a backend that keeps a GPU context
    /// per list runs only the ops added since its last answer.
    #[allow(clippy::too_many_arguments)]
    fn raster_gl(
        &self,
        _cmds: &[GlOp],
        width: u32,
        height: u32,
        _attrs: &GlContextAttributes,
        _params: &RenderParams,
        _list: Option<u64>,
    ) -> Rgba8 {
        Rgba8::transparent(width, height)
    }

    /// The device this backend's WebGL draws on. The default, [`GlDevice::Unknown`], keeps the
    /// persona's recorded values -- a backend with no device of its own has nothing better to say.
    fn gl_device(&self) -> GlDevice {
        GlDevice::Unknown
    }

    /// The page let op list `list` go (its context is gone, or the list started over): whatever the backend keeps
    /// for it -- a GPU context -- goes too.
    fn release_gl(&self, _list: u64) {}

    /// The page made a canvas or asked one for a context: a backend that starts its GPU only when a page needs one
    /// starts it now, ahead of the first GL question.
    fn prepare_gl(&self) {}

    /// Answer a synchronous GL question about the state a command list has built — compile and link
    /// status, info logs, attribute and uniform locations, `getError`, the `is*` predicates.
    ///
    /// This belongs to the backend for the same reason [`RenderBackend::hit_test_2d`] does: the
    /// answer comes from the GLSL compiler and the GL state machine, and a second copy of either in
    /// the recorder would be a second copy that disagrees with the pixels. [`GlQueryResult::None`] is
    /// the null answer, which is also what a backend with no GL gives to everything.
    ///
    /// `list` names the op list `cmds` is: the same `Some` again is the same list, grown
    /// only at its end since, so a backend may go on from the ops it ran for the last question rather
    /// than run them all again. `None` promises nothing.
    #[allow(clippy::too_many_arguments)]
    fn query_gl(
        &self,
        _cmds: &[GlOp],
        _query: &GlQuery,
        _width: u32,
        _height: u32,
        _attrs: &GlContextAttributes,
        _params: &RenderParams,
        _list: Option<u64>,
    ) -> GlQueryResult {
        GlQueryResult::None
    }

    /// Render a recorded audio graph offline and return the interleaved PCM — the Web Audio half of
    /// [`RenderBackend::raster_2d`], and the single source `AudioBuffer.getChannelData` reads.
    ///
    /// `frames` × `channels` samples, channel-interleaved, exactly as an `AudioBuffer` of that shape
    /// holds them. Defaulted to silence so `NullBackend`/`SolidBackend` keep compiling — but a
    /// backend that takes the default is a backend where Web Audio computes nothing, which is
    /// exactly the state R3.5 exists to end. There is deliberately no recorded-tape fallback
    /// anywhere on this path: silence is a visible red line, and a tape is an invisible one.
    fn render_audio(
        &self,
        _graph: &[AudioOp],
        frames: u32,
        channels: u32,
        _sample_rate: f32,
        _params: &RenderParams,
    ) -> Vec<f32> {
        vec![0.0; (frames as usize) * (channels as usize)]
    }

    /// Convert one channel of decoded audio from `from_rate` to `to_rate` as Blink's
    /// `AudioBus::TryCreateBySampleRateConverting` does (its `SincResampler`) -- what `decodeAudioData` does to a
    /// file whose rate is not its context's. `None` = this backend has no resampler (or the length overflows),
    /// and the caller keeps the file's rate.
    fn resample_audio(&self, _samples: &[f32], _from_rate: f64, _to_rate: f64) -> Option<Vec<f32>> {
        None
    }

    /// Answer a synchronous question about a rendered graph — the analyser readbacks and the
    /// compressor's `reduction`.
    ///
    /// This belongs to the backend for the same reason [`RenderBackend::query_gl`] does: the answer
    /// comes out of the same DSP run that produced the destination's samples, so the spectrum a page
    /// reads off an `AnalyserNode` and the samples it reads off the rendered `AudioBuffer` describe
    /// one graph. The 2D path's `getImageData`-real / `toDataURL`-recorded split was a
    /// one-expression tell; this shape has nowhere for it to reappear.
    fn query_audio(
        &self,
        _graph: &[AudioOp],
        _query: &AudioQuery,
        _frames: u32,
        _channels: u32,
        _sample_rate: f32,
        _params: &RenderParams,
    ) -> AudioQueryResult {
        AudioQueryResult::None
    }
}

// ── Web Audio: the R3.5 render graph ──────────────────────────────────────────────────────────

/// Which kind of `AudioNode` an [`AudioOp::Create`] makes. Node id `0` is always the context's
/// `AudioDestinationNode` and is never created explicitly.
///
/// The set here is the set the backend actually renders. A node the DSP has no processor for is
/// deliberately absent rather than present-and-ignored: a variant nothing executes is exactly the
/// fiction this seam exists to prevent (the same rule [`GlOp`] follows for queries and samplers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioNodeKind {
    /// `createOscillator()` — a band-limited periodic source.
    Oscillator,
    /// `createGain()`.
    Gain,
    /// `createDynamicsCompressor()`.
    DynamicsCompressor,
    /// `createAnalyser()` — a pass-through that also taps its input for the FFT readbacks.
    Analyser,
    /// `createBufferSource()`.
    BufferSource,
    /// `createConstantSource()`.
    ConstantSource,
    /// `createDelay(maxDelayTime)` — the maximum rides on [`AudioOp::Create`]'s `arg`.
    Delay,
    /// `createBiquadFilter()`.
    BiquadFilter,
    /// `createStereoPanner()`.
    StereoPanner,
    /// `createWaveShaper()`.
    WaveShaper,
    /// `createChannelMerger(n)` — `n` rides on [`AudioOp::Create`]'s `arg`.
    ChannelMerger,
    /// `createChannelSplitter(n)`.
    ChannelSplitter,
}

/// An `OscillatorNode`'s `type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OscillatorType {
    Sine,
    Square,
    Sawtooth,
    Triangle,
    /// `setPeriodicWave()` — the coefficients ride on [`AudioOp::SetPeriodicWave`].
    Custom,
}

/// A `BiquadFilterNode`'s `type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BiquadKind {
    Lowpass,
    Highpass,
    Bandpass,
    Lowshelf,
    Highshelf,
    Peaking,
    Notch,
    Allpass,
}

/// Which `AudioParam` of a node an [`AudioOp::SetParam`] / [`AudioOp::Automate`] addresses.
///
/// One flat enum rather than one per node kind: the automation timeline is the same machine for
/// every param, and a per-node enum would make the renderer's lookup a two-level match for no
/// information gained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioParamId {
    /// `OscillatorNode` / `BiquadFilterNode`.
    Frequency,
    /// `OscillatorNode` / `AudioBufferSourceNode`, in cents.
    Detune,
    /// `GainNode`, and `BiquadFilterNode`'s shelf/peaking gain.
    Gain,
    /// `DynamicsCompressorNode`.
    Threshold,
    Knee,
    Ratio,
    Attack,
    Release,
    /// `AudioBufferSourceNode`.
    PlaybackRate,
    /// `ConstantSourceNode`.
    Offset,
    /// `DelayNode`.
    DelayTime,
    /// `BiquadFilterNode`.
    Q,
    /// `StereoPannerNode`.
    Pan,
}

/// A non-`AudioParam` property write the renderer has to see.
#[derive(Debug, Clone, PartialEq)]
pub enum AudioProp {
    OscillatorType(OscillatorType),
    BiquadType(BiquadKind),
    /// `AudioBufferSourceNode.loop` / `.loopStart` / `.loopEnd`.
    Loop(bool),
    LoopStart(f64),
    LoopEnd(f64),
    /// `WaveShaperNode.curve` — `None` is the `null` curve, which is a pass-through.
    Curve(Option<Vec<f32>>),
    /// `WaveShaperNode.oversample`: 1 = "none", 2 = "2x", 4 = "4x".
    Oversample(u32),
    /// `AudioNode.channelCount`.
    ChannelCount(u32),
    /// `AudioNode.channelCountMode`: 0 = "max", 1 = "clamped-max", 2 = "explicit".
    ChannelCountMode(u32),
    /// `AudioNode.channelInterpretation`: 0 = "speakers", 1 = "discrete".
    ChannelInterpretation(u32),
    /// `AnalyserNode`.
    FftSize(u32),
    MinDecibels(f64),
    MaxDecibels(f64),
    SmoothingTimeConstant(f64),
}

/// One event on an `AudioParam`'s automation timeline. `time` is context time in seconds.
///
/// The timeline belongs to the CONTRACT rather than to the recorder because the value of a param at
/// sample `n` is a question only the thing computing sample `n` can answer; a second evaluation of
/// the same curve in `webapi` would be a second evaluation, and it would disagree.
#[derive(Debug, Clone, PartialEq)]
pub enum AutomationEvent {
    SetValueAtTime { value: f32, time: f64 },
    LinearRampToValueAtTime { value: f32, time: f64 },
    ExponentialRampToValueAtTime { value: f32, time: f64 },
    SetTargetAtTime { target: f32, time: f64, time_constant: f64 },
    SetValueCurveAtTime { curve: Vec<f32>, time: f64, duration: f64 },
    CancelScheduledValues { time: f64 },
    CancelAndHoldAtTime { time: f64 },
}

/// One recorded Web Audio graph command. `webapi` appends these as the page builds its graph; the
/// active [`RenderBackend`] executes the whole list at readback, exactly as it does for
/// [`Canvas2DOp`] and [`GlOp`].
///
/// Node handles are `u32` ids minted by the recorder; `0` is the context's destination, which every
/// context has from the start and which is therefore never `Create`d.
#[derive(Debug, Clone, PartialEq)]
pub enum AudioOp {
    /// `arg` carries the one constructor scalar a node kind needs and no param models:
    /// `createDelay`'s `maxDelayTime`, `createChannelMerger`/`Splitter`'s width.
    Create { id: u32, kind: AudioNodeKind, arg: f64 },
    SetProp { id: u32, prop: AudioProp },
    SetParam { id: u32, param: AudioParamId, value: f32 },
    Automate { id: u32, param: AudioParamId, event: AutomationEvent },
    /// `setPeriodicWave(wave)` — the Fourier coefficients, index 0 = DC.
    SetPeriodicWave { id: u32, real: Vec<f32>, imag: Vec<f32>, normalize: bool },
    /// `bufferSource.buffer = b` — the PCM the source will play, channel-major.
    SetBuffer { id: u32, channels: Vec<Vec<f32>>, sample_rate: f32 },
    Connect { from: u32, from_output: u32, to: u32, to_input: u32 },
    ConnectParam { from: u32, from_output: u32, to: u32, param: AudioParamId },
    /// `disconnect()` with no argument is `to: None` — every outgoing edge.
    Disconnect { from: u32, to: Option<u32> },
    Start { id: u32, when: f64, offset: f64, duration: Option<f64> },
    Stop { id: u32, when: f64 },
}

/// A synchronous question about a rendered graph — the audio counterpart of [`GlQuery`].
///
/// These exist so `getFloatFrequencyData` and `getChannelData` CANNOT disagree: both answers come
/// out of one run of the same op list in the same backend, the way `readPixels` and `toDataURL`
/// both come out of one `raster_gl`. An FFT computed in `webapi` over samples the backend produced
/// would be a second implementation of "what is this graph doing", and a second implementation is a
/// disagreement waiting for a page to ask twice.
#[derive(Debug, Clone, PartialEq)]
pub enum AudioQuery {
    /// `analyser.getFloatFrequencyData(a)` — `bins` decibel magnitudes.
    FrequencyDb { node: u32, bins: usize },
    /// `analyser.getFloatTimeDomainData(a)` — the most recent `len` samples at the node's input.
    TimeDomain { node: u32, len: usize },
    /// `compressor.reduction` — the gain reduction in dB at the end of the render.
    Reduction { node: u32 },
}

#[derive(Debug, Clone, PartialEq)]
pub enum AudioQueryResult {
    /// No such node, or the backend has no DSP.
    None,
    Samples(Vec<f32>),
    Scalar(f32),
}

// ── WebGL: the R3 command list ────────────────────────────────────────────────────────────────

/// The `WebGLContextAttributes` a context was created with — plain data, the same way
/// [`Canvas2DOp`] is. The backend needs them at raster time (`alpha` decides whether the drawing
/// buffer has an alpha channel at all, `antialias` decides whether the rasterizer supersamples,
/// `preserveDrawingBuffer` decides whether the buffer survives a readback), and the recorder needs
/// them because `getContextAttributes()` reads them back.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GlContextAttributes {
    pub alpha: bool,
    pub depth: bool,
    pub stencil: bool,
    pub antialias: bool,
    pub premultiplied_alpha: bool,
    pub preserve_drawing_buffer: bool,
    pub desynchronized: bool,
    pub fail_if_major_performance_caveat: bool,
    pub power_preference: GlPowerPreference,
    pub xr_compatible: bool,
    /// The context is a WebGL 2 one (an ES 3 context under it, on a GPU). Not a `WebGLContextAttributes` member:
    /// `getContextAttributes()` builds its dictionary by name and never reads it.
    pub webgl2: bool,
}

impl Default for GlContextAttributes {
    /// The WebGL spec's defaults — what a page gets from `getContext("webgl")` with no dictionary.
    fn default() -> Self {
        Self {
            alpha: true,
            depth: true,
            stencil: false,
            antialias: true,
            premultiplied_alpha: true,
            preserve_drawing_buffer: false,
            desynchronized: false,
            fail_if_major_performance_caveat: false,
            power_preference: GlPowerPreference::Default,
            xr_compatible: false,
            webgl2: false,
        }
    }
}

/// Owner 2026-10-12: WebGL on this PC's own GPU, as Chrome draws it: the device a backend's WebGL
/// runs on, as [`RenderBackend::gl_device`] answers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlDevice {
    /// The backend has no device of its own: the persona's recorded values stand.
    Unknown,
    /// This PC has no GPU Chrome 150 would draw WebGL on (none, or a software renderer): no WebGL context at all.
    Absent,
    /// The device's half of the persona's `deviceOracle.webgl` block, as JSON: its limits (`paramsByNum`,
    /// `paramsByNumV2`), shader precision formats, extension lists, sample counts and `WEBGL_debug_renderer_info`
    /// strings, each as ANGLE answers it on this GPU.
    Present(String),
}

/// `WebGLPowerPreference`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum GlPowerPreference {
    #[default]
    Default,
    LowPower,
    HighPerformance,
}

impl GlPowerPreference {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::LowPower => "low-power",
            Self::HighPerformance => "high-performance",
        }
    }
}

/// Which stage a shader object is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GlShaderKind {
    Vertex,
    Fragment,
}

/// A uniform's value, as the page set it. `uniform3fv` and `uniform3f` are the same op here: the
/// recorder normalizes the six argument forms down to one, because the difference between them is a
/// binding-layer detail and the pixels cannot see it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum GlUniformValue {
    /// `uniform{1,2,3,4}f[v]` — `components` is 1..=4 and `values.len()` is a multiple of it (an
    /// array uniform sets several elements in one call).
    Float { components: u8, values: Vec<f32> },
    /// `uniform{1,2,3,4}i[v]` — also the form a `sampler2D` is set with.
    Int { components: u8, values: Vec<i32> },
    /// `uniform{1,2,3,4}ui[v]` (WebGL 2).
    UInt { components: u8, values: Vec<u32> },
    /// `uniformMatrix{2,3,4}fv` — column-major, `rows`x`cols`, `transpose` as the page passed it.
    Matrix { rows: u8, cols: u8, transpose: bool, values: Vec<f32> },
}

/// One recorded WebGL command. The `webapi` front-end appends these as the page draws; the active
/// [`RenderBackend`] executes them at readback, exactly as it does for [`Canvas2DOp`].
///
/// Object handles are `u32` ids minted by the recorder (`0` = the null object / unbind), so this
/// list is plain data end to end — no `deno_core`, no `v8`, no GL crate types. GL *enums* stay as
/// the raw `u32` the page passed, because that is what they are on the wire and remapping 400 of
/// them into Rust enums would only move the match into the recorder.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum GlOp {
    // --- object lifecycle ---
    CreateBuffer { id: u32 },
    DeleteBuffer { id: u32 },
    CreateShader { id: u32, kind: GlShaderKind },
    DeleteShader { id: u32 },
    ShaderSource { shader: u32, source: String },
    CompileShader { shader: u32 },
    CreateProgram { id: u32 },
    DeleteProgram { id: u32 },
    AttachShader { program: u32, shader: u32 },
    DetachShader { program: u32, shader: u32 },
    BindAttribLocation { program: u32, index: u32, name: String },
    LinkProgram { program: u32 },
    ValidateProgram { program: u32 },
    UseProgram { program: u32 },
    CreateTexture { id: u32 },
    DeleteTexture { id: u32 },
    CreateFramebuffer { id: u32 },
    DeleteFramebuffer { id: u32 },
    CreateRenderbuffer { id: u32 },
    DeleteRenderbuffer { id: u32 },
    CreateVertexArray { id: u32 },
    DeleteVertexArray { id: u32 },
    BindVertexArray { id: u32 },

    // --- buffers ---
    BindBuffer { target: u32, buffer: u32 },
    /// `bufferData(target, sizeOrData, usage)`. `data` empty with a non-zero `size` is the
    /// allocate-only form.
    BufferData { target: u32, size: usize, data: Vec<u8>, usage: u32 },
    BufferSubData { target: u32, offset: usize, data: Vec<u8> },

    // --- vertex state ---
    EnableVertexAttribArray { index: u32 },
    DisableVertexAttribArray { index: u32 },
    VertexAttribPointer { index: u32, size: u32, ty: u32, normalized: bool, stride: u32, offset: u32 },
    /// `vertexAttrib{1,2,3,4}f[v]` — the constant value an array-less attribute takes.
    VertexAttrib { index: u32, values: [f32; 4] },
    VertexAttribDivisor { index: u32, divisor: u32 },

    // --- uniforms. Carried by NAME rather than by an opaque location handle: a `WebGLUniformLocation`
    // is per-program and the backend already has the program's declarations, so the name is the one
    // key both ends can agree on without a second id space. ---
    Uniform { program: u32, name: String, value: GlUniformValue },
    /// `transformFeedbackVaryings(program, varyings, bufferMode)`: the varyings the program's NEXT link
    /// captures, and how (`INTERLEAVED_ATTRIBS` / `SEPARATE_ATTRIBS`).
    TransformFeedbackVaryings { program: u32, varyings: Vec<String>, mode: u32 },
    /// `uniformBlockBinding(program, blockIndex, binding)`.
    UniformBlockBinding { program: u32, index: u32, binding: u32 },

    // --- textures ---
    ActiveTexture { unit: u32 },
    BindTexture { target: u32, texture: u32 },
    /// `texImage2D` in both its forms, already decoded to RGBA8 by the recorder (`None` = the
    /// allocate-with-no-data form). `target` distinguishes the six cube faces.
    TexImage2D { target: u32, level: i32, internal_format: u32, width: u32, height: u32, format: u32, ty: u32, pixels: Option<Rgba8> },
    TexSubImage2D { target: u32, level: i32, xoffset: i32, yoffset: i32, format: u32, ty: u32, pixels: Rgba8 },
    /// `texStorage2D` / `texStorage3D` (WebGL 2): immutable storage for `levels` levels of the bound texture, every
    /// one allocated cleared; `depth` is 0 for the 2D form.
    TexStorage { target: u32, levels: i32, internal_format: u32, width: i32, height: i32, depth: i32 },
    TexParameter { target: u32, pname: u32, param: f32 },
    GenerateMipmap { target: u32 },
    PixelStorei { pname: u32, param: i32 },

    // --- framebuffers ---
    BindFramebuffer { target: u32, framebuffer: u32 },
    BindRenderbuffer { target: u32, renderbuffer: u32 },
    RenderbufferStorage { target: u32, internal_format: u32, width: u32, height: u32 },
    FramebufferTexture2D { target: u32, attachment: u32, textarget: u32, texture: u32, level: i32 },
    FramebufferRenderbuffer { target: u32, attachment: u32, rb_target: u32, renderbuffer: u32 },

    // --- fixed-function state ---
    Viewport { x: i32, y: i32, width: i32, height: i32 },
    Scissor { x: i32, y: i32, width: i32, height: i32 },
    ClearColor { r: f32, g: f32, b: f32, a: f32 },
    ClearDepth { depth: f32 },
    ClearStencil { s: i32 },
    Clear { mask: u32 },
    ColorMask { r: bool, g: bool, b: bool, a: bool },
    DepthMask { flag: bool },
    DepthFunc { func: u32 },
    DepthRange { near: f32, far: f32 },
    StencilMask { mask: u32 },
    StencilFunc { face: u32, func: u32, reference: i32, mask: u32 },
    StencilOp { face: u32, fail: u32, zfail: u32, zpass: u32 },
    Enable { cap: u32 },
    Disable { cap: u32 },
    BlendFuncSeparate { src_rgb: u32, dst_rgb: u32, src_alpha: u32, dst_alpha: u32 },
    BlendEquationSeparate { rgb: u32, alpha: u32 },
    BlendColor { r: f32, g: f32, b: f32, a: f32 },
    CullFace { mode: u32 },
    FrontFace { mode: u32 },
    LineWidth { width: f32 },
    PolygonOffset { factor: f32, units: f32 },
    SampleCoverage { value: f32, invert: bool },
    Hint { target: u32, mode: u32 },

    // --- draw ---
    DrawArrays { mode: u32, first: i32, count: i32 },
    DrawElements { mode: u32, count: i32, ty: u32, offset: u32 },
    DrawArraysInstanced { mode: u32, first: i32, count: i32, instances: i32 },
    DrawElementsInstanced { mode: u32, count: i32, ty: u32, offset: u32, instances: i32 },
    Finish,
    Flush,

    /// Raise a GL error the RECORDER detected — an unrecognised `createShader` type, say, which the
    /// front end must answer with `null` before any op could carry it. It exists so there is exactly
    /// ONE error queue: `getError`'s read-and-clear cursor would otherwise have to merge two.
    RaiseError { code: u32 },

    /// The canvas's drawing buffer is reallocated at `width` x `height` and starts CLEARED -- colour
    /// transparent black, depth 1 -- with every other piece of state (viewport and scissor included)
    /// as it was: Blink's `WebGLRenderingContextBase::Reshape` -> `DrawingBuffer::Resize`, which a
    /// canvas whose `width` / `height` CHANGED runs (`HTMLCanvasElement::Reset`: a WebGL canvas
    /// written to the size it already has keeps its pixels). The first op of every list, so a list
    /// carries the size each of its draws went into.
    ResizeDrawingBuffer { width: u32, height: u32 },
    /// `getExtension(name)` enabled the WebGL extension `name` on this context (Blink's `EnsureExtensionEnabled`,
    /// which asks the GPU context for the GL extensions behind it).
    EnableExtension { name: String },
}

/// A synchronous question about the state a [`GlOp`] list has built — the WebGL analogue of
/// [`RenderBackend::hit_test_2d`], and it belongs to the backend for the same reason.
///
/// `getShaderParameter(s, COMPILE_STATUS)` is a question about a GLSL compiler, and the GLSL
/// compiler is in the backend. A second answer computed in `webapi` would be a second compiler that
/// disagrees: a page can compile a shader, read `COMPILE_STATUS` as `true`, link it, draw with it
/// and read the pixels back — and if the recorder said "compiled" while the backend refused it, the
/// pixels contradict the status. One compiler, asked twice.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum GlQuery {
    /// `getShaderParameter(shader, COMPILE_STATUS | SHADER_TYPE | DELETE_STATUS)`.
    ShaderParameter { shader: u32, pname: u32 },
    /// `getShaderInfoLog(shader)`.
    ShaderInfoLog { shader: u32 },
    /// `getShaderSource(shader)`.
    ShaderSource { shader: u32 },
    /// `getProgramParameter(program, LINK_STATUS | ACTIVE_ATTRIBUTES | ...)`.
    ProgramParameter { program: u32, pname: u32 },
    /// `getProgramInfoLog(program)`.
    ProgramInfoLog { program: u32 },
    /// `getAttachedShaders(program)` — the shader ids, in attach order.
    AttachedShaders { program: u32 },
    /// `getAttribLocation(program, name)` — `-1` when the name is not an active attribute.
    AttribLocation { program: u32, name: String },
    /// `getUniformLocation(program, name)` — the null return is [`GlQueryResult::None`].
    UniformLocation { program: u32, name: String },
    /// `getActiveAttrib(program, index)`.
    ActiveAttrib { program: u32, index: u32 },
    /// `getActiveUniform(program, index)`.
    ActiveUniform { program: u32, index: u32 },
    /// `getUniform(program, name)` — the value currently set for that uniform.
    Uniform { program: u32, name: String },
    /// `getError()` — the `consumed`-th error the list raised, `0` (`NO_ERROR`) once they run out.
    ///
    /// GL's error flags are read-and-clear, and this seam replays the whole list on every call, so
    /// the caller carries the read cursor: it counts how many non-zero answers it has already handed
    /// the page, and the backend returns the next one.
    Error { consumed: u32 },
    /// `readPixels(x, y, width, height, ...)` — a region of the CURRENTLY BOUND framebuffer, in GL's
    /// own bottom-left row order, RGBA8. Separate from `raster_gl` (which is always the default
    /// framebuffer, because that is what `toDataURL` serializes) but computed by the same replay, so
    /// the two cannot disagree about the default framebuffer's pixels.
    ReadPixels { x: i32, y: i32, width: u32, height: u32 },
    /// `isBuffer` / `isShader` / `isProgram` / `isTexture` / `isFramebuffer` / `isRenderbuffer` /
    /// `isVertexArray`.
    IsObject { kind: GlObjectKind, id: u32 },
    /// `isEnabled(cap)`.
    IsEnabled { cap: u32 },
    /// `getBufferParameter(target, pname)`.
    BufferParameter { target: u32, pname: u32 },
    /// `getVertexAttrib(index, pname)`.
    VertexAttrib { index: u32, pname: u32 },
    /// `getVertexAttribOffset(index, pname)`.
    VertexAttribOffset { index: u32, pname: u32 },
    /// `getTexParameter(target, pname)`.
    TexParameter { target: u32, pname: u32 },
    /// `checkFramebufferStatus(target)`.
    FramebufferStatus { target: u32 },
    /// `getRenderbufferParameter(target, pname)`.
    RenderbufferParameter { target: u32, pname: u32 },
    /// `getFramebufferAttachmentParameter(target, attachment, pname)`.
    FramebufferAttachmentParameter { target: u32, attachment: u32, pname: u32 },
    /// The GL type enum of the uniform `name` addresses (`FLOAT_VEC4`, `UNSIGNED_INT_VEC3`, ...) --
    /// what `uniform*` checks its spelling against. [`GlQueryResult::None`] for no such uniform.
    UniformType { program: u32, name: String },
    /// `getUniformBlockIndex(program, name)` -- `INVALID_INDEX` (0xFFFFFFFF) when there is none.
    UniformBlockIndex { program: u32, name: String },
    /// `getActiveUniformBlockName(program, index)`.
    UniformBlockName { program: u32, index: u32 },
    /// `getActiveUniformBlockParameter(program, index, pname)`.
    UniformBlockParameter { program: u32, index: u32, pname: u32 },
    /// `getUniformIndices(program, names)` -- one index per name, `INVALID_INDEX` for an unknown one.
    UniformIndices { program: u32, names: Vec<String> },
    /// `getActiveUniforms(program, indices, pname)` -- one answer per index.
    ActiveUniforms { program: u32, indices: Vec<u32>, pname: u32 },
    /// `getFragDataLocation(program, name)` -- `-1` when `name` is not a fragment output.
    FragDataLocation { program: u32, name: String },
    /// `getTransformFeedbackVarying(program, index)`.
    TransformFeedbackVarying { program: u32, index: u32 },
    /// A `getParameter` name Chrome's GL client does not answer from its own cache (`GLES2Implementation::GetHelper`;
    /// `GetString` never does): Chrome sends `glGet*(pname)` / `glGetString(pname)` to its GPU process and waits for
    /// the answer, and so does the binding -- a GPU backend reads `pname` and answers it (a `GL_*` name, the
    /// `WEBGL_debug_renderer_info` ones already mapped to `GL_VENDOR` / `GL_RENDERER`).
    Get { pname: u32 },
    /// `getInternalformatParameter(target, internalformat, pname)`: `glGetInternalformativ`, a GPU-process round trip.
    Internalformat { target: u32, internal_format: u32, pname: u32 },
}

/// Which object table a [`GlQuery::IsObject`] is asking about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GlObjectKind {
    Buffer,
    Shader,
    Program,
    Texture,
    Framebuffer,
    Renderbuffer,
    VertexArray,
}

/// What a [`GlQuery`] answers with. `None` is WebGL's `null` — an unknown name, a deleted object, a
/// query the backend does not model.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum GlQueryResult {
    None,
    Bool(bool),
    Int(i32),
    UInt(u32),
    Float(f32),
    Str(String),
    Floats(Vec<f32>),
    Ints(Vec<i32>),
    Bools(Vec<bool>),
    /// Unsigned integers -- a `uvec*` uniform's value, a list of uniform indices.
    UInts(Vec<u32>),
    /// `getActiveAttrib` / `getActiveUniform` — a `WebGLActiveInfo`'s three fields.
    ActiveInfo { name: String, size: i32, ty: u32 },
    /// `readPixels` — RGBA8, bottom-left row order.
    Bytes(Vec<u8>),
}

/// A canvas font, as [`RenderBackend::text_ink`] resolves it.
#[derive(Debug, Clone, PartialEq)]
pub struct TextFontRequest {
    /// The CSS `font-family` list, as written (`"Arial, sans-serif"`).
    pub families: String,
    /// The family a list naming no available font falls back to (the persona's default face).
    pub default_family: String,
    /// The computed size in px.
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    /// The language the text is shaped in (the UI locale: `ko-KR`), which picks fontconfig's choice
    /// for a generic family and the font's `locl` forms.
    pub lang: String,
    /// Shaped word by word, as Blink's canvas text is (`PlainTextNode::SegmentWord`: no kerning pair spans a
    /// space); `false` shapes the text as one item, as layout's inline text is (`InlineNode::ShapeText` -- an
    /// SVG `<text>`).
    pub words: bool,
}

/// A font file's metrics as layout measures text in it ([`RenderBackend::web_font_metrics`]), in ems: what
/// `korvus-page`'s captured `FontMetrics` holds for an installed face, read off the file instead (skrifa's metrics,
/// which Chrome 152 on Linux reads every font with; HarfBuzz's pair adjustments).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FontFileMetrics {
    /// The family name the file gives itself (`name` ID 1).
    pub family: String,
    pub ascent: f32,
    pub descent: f32,
    pub line_gap: f32,
    pub x_height: f32,
    pub cap_height: f32,
    /// The advance of `0`.
    pub zero_advance: f32,
    /// OS/2 `xAvgCharWidth`, and the `head` bounding box's width.
    pub avg_char_width: Option<f32>,
    pub max_char_width: Option<f32>,
    /// Printable ASCII (U+0020 to U+007E), in order; a character the font has no glyph for is 0 and its bit of
    /// `covered` (bit `c - 0x20`) clear.
    pub advances: Vec<f32>,
    pub covered: u128,
    /// The printable-ASCII pairs whose advance shaping adjusts, sorted.
    pub kerning: Vec<(char, char, f32)>,
    /// The font's own non-ASCII Latin, punctuation, currency and symbol characters, sorted.
    pub extra: Vec<(char, f32)>,
    /// One advance per script class (Hangul, Jamo, CJK, Kana, CJK punctuation, emoji), 0 where the font has none.
    pub class_advances: [f32; 6],
    /// A variable font's `wght` axis, its least and greatest values: such a font is measured (and drawn) per weight,
    /// under the instance name `<family>@<weight>`. `None` for a font without one.
    pub weight_axis: Option<(f32, f32)>,
    /// Its `opsz` axis: such a font is measured (and drawn) per size too, `<family>@<weight>@<size>` -- Chrome sets the
    /// optical size to the font size (`font-optical-sizing: auto`). `None` for a font without one.
    pub opsz_axis: Option<(f32, f32)>,
}

/// A font file a `local()` name matched ([`RenderBackend::local_font`]): the file, and the face's index in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalFont {
    pub path: String,
    pub index: u32,
}

/// What [`RenderBackend::text_ink`] measures.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TextInk {
    /// The union of the glyphs' ink boxes in px, each rounded out to whole pixels at its glyph's
    /// origin and offset by that glyph's pen position, from the run's start on the alphabetic
    /// baseline, y down: `[left, top, right, bottom]`. `None` when no glyph has ink.
    pub bounds: Option<[f32; 4]>,
    /// The run's advance in px, each glyph's 16.16 advance summed in `float` as Blink sums them -- in
    /// the fonts the glyphs are actually in, fallback fonts included.
    pub width: f32,
    /// The primary font's units per em and `OS/2` `sTypoAscender` / `-sTypoDescender` (font units).
    pub units_per_em: u16,
    pub typo: Option<(i16, i16)>,
    /// The primary font's `BASE` horizontal baselines for the default script, in px above the
    /// alphabetic one as HarfBuzz scales them (`em_mult` at Blink's 16.16 size): `romn`, `hang`, `ideo`.
    pub base: [Option<f32>; 3],
    /// The caret position before each UTF-16 code unit of the text and after its last, in px: the advances of
    /// the glyphs of the clusters before it, summed in `float` in the WHOLE run's shaping -- a kerning pair's
    /// adjustment is its first glyph's, as Blink's `ShapeResult::CaretPositionForOffset` reads it
    /// (an SVG text's per-character extents). Empty when the backend does not shape.
    pub carets: Vec<f32>,
    /// The primary font's ascent and descent in px, rounded as Blink's `SimpleFontData` rounds them on Linux --
    /// the box an SVG text's characters stand in (`FixedAscent` / `FixedDescent`), from the face the glyphs were
    /// shaped in. `None` when the backend does not shape.
    pub metrics: Option<(f32, f32)>,
}

/// The serializations `canvas.toDataURL` / `toBlob` can be asked for. Anything else is PNG, per HTML.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Webp,
}

/// The render **service** as it lives on the kernel [`kernel::Context`]: the active [`RenderBackend`],
/// provided by a render plugin and consumed by the canvas/WebGL bindings via `ctx.get::<RenderHandle>()`.
/// `Arc` so the `Context` shares it cheaply. A contract type (no engine deps — korvus-kernel stays
/// contract-only); the plugin bridge lives in `korvus-runtime`.
pub struct RenderHandle(pub std::sync::Arc<dyn RenderBackend>);

/// The pixel engine that paints the **document** into frames — the compositor, as distinct from
/// [`RenderBackend`] (which rasters a single canvas/WebGL/audio display list). The `page` crate
/// implements it (RC1+: box tree → display list → layer tree, rastered via [`RenderHandle`]'s Skia);
/// `Engine::pump`'s rendering opportunity calls [`Compositor::update_rendering`]. The default is
/// [`NullCompositor`] — no paint, which is Korvus's behaviour before the compositor lands and the
/// zero-cost default for any page that needs no frame. A contract type:
/// no engine deps, korvus-kernel stays contract-only; the plugin bridge lives in `korvus-runtime`.
pub trait Compositor: Send + Sync {
    /// Human-readable backend name (e.g. "null", "page").
    fn name(&self) -> &str;

    /// A rendering opportunity has arrived: paint the document into a frame. The default is a no-op —
    /// the null compositor, and what every compositor inherits until it paints. RC1+ extends the
    /// signature with the box-tree read handle and the [`RenderParams`] the paint walk needs.
    fn update_rendering(&self) {}
}

/// The compositor **service** on the kernel [`kernel::Context`], provided by a plugin and read via
/// `ctx.get::<CompositorHandle>()` — the same shape as [`RenderHandle`]. `Arc` so the `Context` shares
/// it cheaply; a contract type, no engine deps.
pub struct CompositorHandle(pub std::sync::Arc<dyn Compositor>);

/// No-op compositor: paints nothing. Korvus's behaviour before the document compositor lands, the
/// default service, and the behaviour every compositor inherits from the trait by default.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullCompositor;

impl Compositor for NullCompositor {
    fn name(&self) -> &str {
        "null"
    }
}

/// Text measurement, as a service on the kernel [`kernel::Context`].
///
/// `ctx.measureText("Hello").width` and the `offsetWidth` of a `<span>` containing "Hello" are the
/// same question asked twice, and a page can ask both. Routing the canvas through the same engine
/// that lays out text is what keeps the two from disagreeing — a disagreement is not a rounding
/// error, it is a cross-check a detector runs in three lines.
///
/// A contract here rather than a direct call because the two live in different plugins: the canvas
/// bindings must not depend on the page pipeline's crate, only on this one.
pub trait CssValues: Send + Sync {
    /// Width, ascent, and descent of `text` in `families` at `size_px`, in CSS pixels.
    ///
    /// Three floats rather than a struct so the contract carries no type the caller has to name.
    fn measure_text(&self, families: &str, size_px: f32, text: &str) -> [f32; 3];

    /// [`CssValues::measure_text`] in the style a bold weight (600 or more) and an italic or oblique
    /// slant pick -- `ctx.font = "bold 16px Arial"` measures the bold face. A provider
    /// with one style per face answers the plain measurement.
    fn measure_text_styled(&self, families: &str, size_px: f32, bold: bool, italic: bool, text: &str) -> [f32; 3] {
        let _ = (bold, italic);
        self.measure_text(families, size_px, text)
    }

    /// [`CssValues::measure_text_styled`] with the fonts of the bundle [`ThemeProfile::font_map`]
    /// names. A provider with one set of fonts answers the plain measurement.
    fn measure_text_in(&self, font_map: u64, families: &str, size_px: f32, bold: bool, italic: bool, text: &str) -> [f32; 3] {
        let _ = font_map;
        self.measure_text_styled(families, size_px, bold, italic, text)
    }

    /// The family a `font-family` list naming nothing installed falls back to -- the persona's default
    /// face (the render backend measures ink in the same font this measures in).
    fn default_family(&self, font_map: u64) -> String {
        let _ = font_map;
        "sans-serif".into()
    }

    /// A CSS color as `[r, g, b, a]`, or `None` if it is not one.
    ///
    /// Here for the same reason as the measurement above: `ctx.fillStyle = "rebeccapurple"` and
    /// `getComputedStyle(el).color` are the same parser, and a canvas that knew seven color names
    /// while the cascade knew a hundred and forty-eight is a difference a page can read.
    fn parse_color(&self, text: &str) -> Option<[u8; 4]>;

    /// The same colour, keeping the alpha CSS actually parsed instead of an 8-bit channel.
    ///
    /// `ctx.fillStyle` SERIALIZES the alpha, so the two are not equivalent: `rgba(0, 0, 0, 0.5)`
    /// reads back as `rgba(0, 0, 0, 0.5)` in a browser and as `0.502` through an 8-bit round-trip
    /// (128/255). One decimal place on the most-hashed surface on the web is worth a second method.
    /// The default derives it from [`Self::parse_color`], so an implementation that has no float to
    /// give is still correct, just quantized.
    fn parse_color_alpha(&self, text: &str) -> Option<([u8; 3], f32)> {
        let c = self.parse_color(text)?;
        Some(([c[0], c[1], c[2]], c[3] as f32 / 255.0))
    }

    /// A colour string as a canvas takes it -- Blink's `ParseCanvasColorString`: CSS Color 4/5
    /// in full (`color(display-p3 ...)`, `lab()`, `oklch()`, `color-mix()`, relative colours), kept as Blink keeps it,
    /// painted with its `toSkColor4f` and read back with its `SerializeAsCanvasColor`. `element` = the canvas is a
    /// `<canvas>` (Blink resolves a system colour through the document's colour provider, which an `OffscreenCanvas`
    /// has none of); `current` = the CSS text of the colour `currentcolor` means there (a connected `<canvas>`'s
    /// inline `color`), black when `None`. The default is the 8-bit parse above, which knows no wide-gamut colour.
    fn parse_canvas_color(&self, text: &str, element: bool, current: Option<&str>) -> Option<CanvasColor> {
        let _ = element;
        let text = if text.trim().eq_ignore_ascii_case("currentcolor") { current.unwrap_or("#000") } else { text };
        let (rgb, a) = self.parse_color_alpha(text)?;
        let text = if a >= 1.0 { format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]) } else { format!("rgba({}, {}, {}, {a})", rgb[0], rgb[1], rgb[2]) };
        let paint = legacy_color4(rgb[0], rgb[1], rgb[2], a);
        Some(CanvasColor { paint, text, oklab_stop: paint, legacy: true })
    }
}

/// A colour as a canvas holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct CanvasColor {
    /// What it paints with: Blink's `toSkColor4f`.
    pub paint: Color4f,
    /// What `fillStyle` / `strokeStyle` / `shadowColor` read back: `SerializeAsCanvasColor`.
    pub text: String,
    /// As a stop of a gradient Blink interpolates in OKLab: converted to OKLab and back to sRGB floats.
    pub oklab_stop: Color4f,
    /// Interpolated as a legacy colour (`GetColorInterpolationSpace() == kSRGBLegacy`).
    pub legacy: bool,
}



/// The CSS value service: the layer the cascade and the canvas share. `None` on the `Context` means
/// no page pipeline is loaded, and the caller falls back to whatever it did before — see
/// [`RenderHandle`] for the same shape.
pub struct CssHandle(pub std::sync::Arc<dyn CssValues>);

/// The persona **service** on the kernel [`kernel::Context`]: the device identity, provided by a persona
/// plugin and read (today, via a bridge) by the navigator/screen/UA-CH bindings. `Arc` so the `Context`
/// shares it and a community plugin can swap/rotate it. A contract type (no engine deps).
pub struct PersonaHandle(pub std::sync::Arc<dyn Persona>);

/// The realm's high-resolution monotonic clock — the one `performance.now()` and
/// `performance.timeOrigin` read, and the one `document.timeline.currentTime` follows.
///
/// **Here, and not in the engine, because two crates need the same clock.** `korvus-runtime` creates
/// one per realm at extension-init time (so `timeOrigin` is when the realm started, not when a page
/// first asked); the `Performance` table in `korvus-webapi` reads it out of `OpState` at CALL time.
/// A second `Instant` anywhere would drift from it, and `document.timeline.currentTime -
/// performance.now()` is a two-line probe.
///
/// **The 100 µs quantization is a fingerprinting decision, not a resolution bug.** `Date.now()`-based
/// timing has 1 ms resolution, so rapid `performance.now()` calls return the SAME value — a glaring
/// tell, since a real `performance.now()` is sub-ms and strictly advancing. This reads a monotonic
/// `Instant` and clamps to 100 µs, exactly like Chrome's Spectre mitigation. Do not "improve" it.
///
/// **And the quantum is only half of it: the ARITHMETIC has a shape**. Clamping to
/// 100 µs and dividing leaves an exact decimal multiple of 0.1, so `String(performance.now())` is
/// always one decimal place — `"1588.7"`. A browser's is not. Measured 2026-09-18, 600 readings each
/// from Chrome 150.0.7871.46 and Brave 151.1.93.138 on this box: **every reading is a multiple of the
/// ULP OF THE MONOTONIC CLOCK'S OWN MILLISECOND VALUE**, `1024.7000000029802`, `17.600000008940697`,
/// `1041.0999999940395`. That is what [`Self::dom_hrt`] reproduces, and the reason this struct now
/// carries [`PerfClock::start_abs_us`]. See it for the derivation.
///
/// `Copy`: immutable once made, so a reader may keep its own copy (`korvus-webapi` keeps one in an
/// isolate slot, read on every `performance.now()`).
#[derive(Clone, Copy)]
pub struct PerfClock {
    start: std::time::Instant,
    /// The monotonic clock's OWN reading at `start`, in microseconds — how far the platform's
    /// boot-relative monotonic clock had already counted when this realm began. It is not a
    /// decoration: [`Self::dom_hrt`] subtracts two doubles taken at THIS magnitude, and the magnitude
    /// is what gives the result its float shape.
    start_abs_us: i64,
    /// The wall-clock epoch microsecond of the origin, unclamped: `timeOrigin` is it through the clamper at the
    /// clock's resolution, as Blink's `Performance::timeOrigin` clamps `unix_at_zero_monotonic + origin`.
    origin_wall_us: i64,
    /// The clamper's secret ([`clamper_secret`]: one per process, as Blink's `static TimeClamper`).
    secret: u64,
    /// [`COARSE_RESOLUTION_US`], or [`FINE_RESOLUTION_US`] once the realm is known to be
    /// cross-origin isolated ([`Self::set_cross_origin_isolated`]).
    resolution_us: i64,
    /// korvus-analysis T: the recorder's time already spent when this clock was made. A realm made
    /// mid-run (a worker, a frame) leaves out only what the recorder spends from then on -- it took the
    /// whole run's, so a challenge worker started at 1.06 s of a traced run read `performance.now()`
    /// -377 ms.
    #[cfg(feature = "analysis")]
    rec0_ns: u64,
}

/// The resolution `performance.now()` is clamped to, in microseconds — Chromium's
/// `TimeClamper::kCoarseResolutionMicroseconds`.
///
/// Chromium has two: 5 µs for a **cross-origin-isolated** agent cluster and 100 µs for everything
/// else. The coarse one is every clock's start: confirmed in the 2026-09-18 sample, every one of the
/// 1,200 readings across the two browsers (on a page that was not isolated) had `now * 10` an
/// integer. Brave adds no second clamp of its own on top.
const COARSE_RESOLUTION_US: i64 = 100;

/// `TimeClamper::kFineResolutionMicroseconds`: the clamp of a cross-origin-isolated realm.
/// Chrome 150 on `H-coi` (COOP same-origin + COEP require-corp): the smallest step a challenge provider's probe
/// finds is 0.005 ms in the page, its worker and a COEP iframe -- and 0.1 on the same page without the
/// headers. a managed bot challenge serves both headers and measures it in a worker.
const FINE_RESOLUTION_US: i64 = 5;

impl Default for PerfClock {
    fn default() -> Self {
        let start = std::time::Instant::now();
        // `timeOrigin` is 100 µs-quantized too, and korvus's was not: `1789718898281.8726` against
        // Chrome's `1789718996556.8` and Brave's `1789718997834.7` (five launches, every one a single
        // decimal place). At this magnitude a double's ULP is ~244 ns, so the extra digits korvus
        // printed were REAL sub-quantum precision, readable with one `String()`.
        // (on Blink's clamper: [`PerfClock::origin_ms`])
        let origin_wall_us = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros() as i64)
            .unwrap_or(0);
        PerfClock {
            start,
            start_abs_us: abs_us(start),
            origin_wall_us,
            secret: clamper_secret(),
            resolution_us: COARSE_RESOLUTION_US,
            #[cfg(feature = "analysis")]
            rec0_ns: crate::analysis::recorder_ns(),
        }
    }
}

impl PerfClock {
    /// A clock whose origin is the process-clock reading `process` (a [`process_ms`] value) in the PAST --
    /// a document's `timeOrigin` is its NAVIGATION's start, before the response arrived (HR Time; ACCESS
    /// 2026-10-04). A reading in the future is taken as now.
    pub fn since_process_ms(process: f64) -> Self {
        let now = std::time::Instant::now();
        let back_us = ((process_ms(now) - process) * 1000.0).max(0.0) as u64;
        let start = now.checked_sub(std::time::Duration::from_micros(back_us)).unwrap_or(now);
        let wall_us = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros() as i64)
            .unwrap_or(0)
            - back_us as i64;
        PerfClock {
            start,
            start_abs_us: abs_us(start),
            origin_wall_us: wall_us,
            secret: clamper_secret(),
            resolution_us: COARSE_RESOLUTION_US,
            #[cfg(feature = "analysis")]
            rec0_ns: crate::analysis::recorder_ns(),
        }
    }

    /// The realm's isolation, decided by its document's response (or a worker's creator) after the
    /// clock started: an isolated realm's readings clamp to Chromium's fine 5 µs. The origin keeps
    /// its 100 µs quantum, which is a multiple of 5 µs, so both stay on the clamp's grid.
    pub fn set_cross_origin_isolated(&mut self, isolated: bool) {
        self.resolution_us = if isolated { FINE_RESOLUTION_US } else { COARSE_RESOLUTION_US };
    }
    /// Whether the realm clamps at the cross-origin-isolated precision (a frame's clock takes its page's).
    pub fn cross_origin_isolated(&self) -> bool {
        self.resolution_us == FINE_RESOLUTION_US
    }
    /// `performance.now()` — monotonic ms since this realm's origin, quantized to 100 µs, through
    /// Chromium's arithmetic ([`Self::dom_hrt`]).
    #[cfg(not(feature = "analysis"))]
    pub fn now_ms(&self) -> f64 {
        self.dom_hrt(self.start.elapsed().as_micros() as i64)
    }
    /// korvus-analysis T: a traced run's clock leaves the recorder's own time out -- what it spent since
    /// this clock was made (a body of its own, so the release body stays exactly as it was -- A3).
    #[cfg(feature = "analysis")]
    pub fn now_ms(&self) -> f64 {
        let spent_us = (crate::analysis::recorder_ns().saturating_sub(self.rec0_ns) / 1000) as i64;
        self.dom_hrt(self.start.elapsed().as_micros() as i64 - spent_us)
    }
    /// `performance.timeOrigin` — the wall-clock epoch ms at which the origin was taken, on the same
    /// 100 µs quantum.
    pub fn origin_ms(&self) -> f64 {
        clamp_us(self.origin_wall_us, self.resolution_us, self.secret) as f64 / 1000.0
    }
    /// A time on this realm's timeline -- ms from its origin, unclamped (a clamped time plus a raw distance) -- as a
    /// `DOMHighResTimeStamp`: clamped as the absolute time it is ([`Self::dom_hrt`]).
    pub fn clamped_ms(&self, rel_ms: f64) -> f64 {
        self.dom_hrt((rel_ms * 1000.0).round() as i64)
    }
    /// A [`process_ms`] reading as this realm's `DOMHighResTimeStamp`, quantized like [`Self::now_ms`].
    /// This is how a time the TRANSPORT measured (on its own thread, with no realm in reach) becomes a
    /// resource-timing value the page can compare with `performance.now()`.
    pub fn realm_ms(&self, process: f64) -> f64 {
        self.dom_hrt(((process - process_ms(self.start)) * 1000.0).round() as i64)
    }

    /// `chrome.csi().pageT` — which is **not** a `DOMHighResTimeStamp` and must not be shaped like
    /// one.
    ///
    /// Chromium's legacy `loadtimes_extension_bindings` computes it off the WALL clock:
    /// `base::Time::Now() - navigationStart`, reported through `TimeDelta::InMillisecondsF()`. That is
    /// microsecond resolution with no Spectre clamp anywhere near it — the binding predates the clamp
    /// and never got one. Measured 2026-09-18, twenty readings per engine:
    ///
    /// ```text
    /// chrome 150   now 1041.0999999940395   pageT 1041.847   startE 1789718996556
    /// brave  151   now 1035.9000000059605   pageT 1036.562   startE 1789718997834
    /// korvus       now  318.40000000596046  pageT  318.5     <- the monotonic clock, clamped
    /// ```
    ///
    /// Two tells in one number: three decimal places where korvus had one, and the OFFSET from
    /// `now()` — `pageT` is measured from the FLOORED `navigationStart`, so it carries the origin's
    /// sub-millisecond fraction (0.75 ms in the Chrome column, 0.66 in Brave's) that `now()` does not.
    pub fn legacy_page_ms(&self) -> f64 {
        let now_us = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros() as i64)
            .unwrap_or(0);
        let nav_start_us = (self.origin_ms().floor() as i64).saturating_mul(1000);
        (now_us - nav_start_us) as f64 / 1000.0
    }

    /// One elapsed-microsecond count as a `DOMHighResTimeStamp`, **the way Chromium computes one**.
    ///
    /// The clamp is Blink's `TimeClamper` ([`clamp_us`]): each ABSOLUTE time -- the reading and the
    /// origin -- goes to its tick or the next, unbiased, and the two are subtracted as doubles
    /// (`Performance::MonotonicTimeToDOMHighResTimeStamp`). It was a floor of the elapsed time, which read every
    /// timestamp ~50 µs low and any interval under a tick as exactly 0. The other half korvus was missing is
    /// that Chromium never has an exact multiple of 0.1 ms in its hands. Its `TimeTicks` are
    /// boot-relative, and the timestamp a page reads is the DIFFERENCE OF TWO DOUBLES taken on that
    /// boot-relative millisecond scale — the realm's origin and the reading. On a box a day into its
    /// uptime that scale is ~1.0e8 ms, where a double's ULP is 2⁻²⁶ ms ≈ 1.49e-8, and both endpoints
    /// snap to that grid before they are subtracted. The 100 µs tick survives as the NOMINAL value and
    /// the grid shows up as a deterministic residue around it:
    ///
    /// ```text
    /// brave 151   1021.9000000059605  1022.2999999970198  1022.7000000029802  1023.0999999940395
    /// chrome 150  1037.9000000059605  1038.300000011921   1038.7000000029802  1039.1000000089407
    /// korvus      1024.7              1024.8              1024.9              1025
    /// ```
    ///
    /// The residue takes **exactly five values**, because the k-th tick sits at `0.4k mod 1` grid
    /// steps and the origin's own rounding shifts all five by one constant — so one reading in five
    /// (`k ≡ 0 mod 5`, the ticks that land on a multiple of 0.5 ms) is still exact and the other four
    /// are not. That is the shape, and it falls out of doing the same arithmetic rather than from any
    /// added noise: this function invents nothing, it just stops discarding the origin's magnitude.
    fn dom_hrt(&self, raw_us: i64) -> f64 {
        let now = clamp_us(self.start_abs_us.saturating_add(raw_us), self.resolution_us, self.secret) as f64 / 1000.0;
        let origin = clamp_us(self.start_abs_us, self.resolution_us, self.secret) as f64 / 1000.0;
        now - origin
    }
}

/// Blink's `TimeClamper::ClampTimeResolution` (chromium-150 `third_party/blink/renderer/core/timing/time_clamper.cc`),
/// on microseconds, line for line: the lower ten decimal digits are clamped apart from the upper ones; the tick goes
/// UP when the lower digits reach `tick + resolution * U`, `U` in [0, 1) from MurmurHash3 of the tick ^ `secret`; a
/// negative time is clamped as its magnitude. So a time a fraction `f` into its tick reads the next tick with
/// probability `f`, and every reading in one tick goes the same way (monotonic).
fn clamp_us(time_us: i64, resolution_us: i64, secret: u64) -> i64 {
    const TEN_LOWER_DIGITS_MOD: i64 = 10_000_000_000;
    let negative = time_us < 0;
    let t = if negative { -(time_us.max(-i64::MAX)) } else { time_us };
    let lower = t % TEN_LOWER_DIGITS_MOD;
    let upper = t - lower;
    let mut clamped = lower - lower % resolution_us;
    let threshold = clamped as f64 + resolution_us as f64 * murmur_to_unit(murmur3((clamped as u64) ^ secret));
    if lower as f64 >= threshold {
        clamped += resolution_us;
    }
    clamped += upper.min(i64::MAX - clamped);
    if negative { -clamped } else { clamped }
}

/// A SPAN of `us` microseconds through this process's clamper, in ms -- Blink's `Performance::ClampTimeResolution` of a
/// `TimeDelta` that is no point on a timeline (`IdleDeadline::timeRemaining`), 5 µs when cross-origin isolated.
pub fn clamp_span_ms(us: i64, cross_origin_isolated: bool) -> f64 {
    let res = if cross_origin_isolated { FINE_RESOLUTION_US } else { COARSE_RESOLUTION_US };
    clamp_us(us, res, clamper_secret()) as f64 / 1000.0
}

/// `TimeClamper::MurmurHash3` (the 64-bit finaliser).
fn murmur3(mut v: u64) -> u64 {
    v ^= v >> 33;
    v = v.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    v ^= v >> 33;
    v = v.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
    v ^= v >> 33;
    v
}

/// `TimeClamper::ToDouble`: the mantissa bits as a double in [1, 2), minus 1.
fn murmur_to_unit(v: u64) -> f64 {
    f64::from_bits((v & 0x000F_FFFF_FFFF_FFFF) | 0x3FF0_0000_0000_0000) - 1.0
}

/// The clamper's secret: random, one per process (Blink: `static TimeClamper clamper` in
/// `Performance::ClampTimeResolution`, `secret_(base::RandUint64())`), shared by every realm and worker of it.
fn clamper_secret() -> u64 {
    static SECRET: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *SECRET.get_or_init(|| {
        use std::hash::{BuildHasher, Hasher};
        // std's RandomState keys come from the OS's randomness once per thread: a hash under them is a random u64
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(std::process::id() as u64);
        h.finish()
    })
}

/// Milliseconds of `t` on ONE process-wide monotonic clock (its zero is the first reading), signed so an
/// instant from before the zero is negative. Every realm's [`PerfClock`] converts from it with
/// [`PerfClock::realm_ms`].
pub fn process_ms(t: std::time::Instant) -> f64 {
    let (zero, _) = process_zero();
    match t.checked_duration_since(zero) {
        Some(d) => d.as_secs_f64() * 1000.0,
        None => -(zero.duration_since(t).as_secs_f64() * 1000.0),
    }
}

/// The instant [`process_ms`] reads as `ms`.
pub fn process_instant(ms: f64) -> std::time::Instant {
    let (zero, _) = process_zero();
    let d = std::time::Duration::from_secs_f64(ms.abs() / 1000.0);
    if ms >= 0.0 { zero + d } else { zero.checked_sub(d).unwrap_or(zero) }
}

/// `t` on the platform's own monotonic clock, in microseconds — the absolute reading
/// [`PerfClock::dom_hrt`] needs, which `std::time::Instant` deliberately does not expose.
fn abs_us(t: std::time::Instant) -> i64 {
    let (zero, zero_abs_us) = process_zero();
    let since_zero = match t.checked_duration_since(zero) {
        Some(d) => d.as_micros() as i64,
        None => -(zero.duration_since(t).as_micros() as i64),
    };
    zero_abs_us.saturating_add(since_zero)
}

/// The process-wide monotonic zero: the first [`std::time::Instant`] anything asked for, and what the
/// platform's monotonic clock read at that moment.
fn process_zero() -> (std::time::Instant, i64) {
    static ZERO: std::sync::OnceLock<(std::time::Instant, i64)> = std::sync::OnceLock::new();
    *ZERO.get_or_init(|| (std::time::Instant::now(), monotonic_epoch_us()))
}

/// How far the platform's monotonic clock has already counted, in microseconds, right now.
///
/// Every platform Chromium runs on measures `TimeTicks` from boot — `CLOCK_MONOTONIC`,
/// `mach_absolute_time`, `QueryPerformanceCounter` — so "since boot" is the quantity, and on Linux
/// `/proc/uptime`'s first field is that clock's own reading in seconds.
fn monotonic_epoch_us() -> i64 {
    #[cfg(target_os = "linux")]
    if let Ok(s) = std::fs::read_to_string("/proc/uptime") {
        if let Some(secs) = s.split_whitespace().next().and_then(|f| f.parse::<f64>().ok()) {
            return (secs * 1_000_000.0) as i64;
        }
    }
    // No readable monotonic epoch. What shapes the double is the MAGNITUDE, not the number, so a
    // stand-in of the same order does the job: the wall clock folded into a week is neither a
    // constant nor an implausible uptime.
    let epoch_us = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0);
    epoch_us.rem_euclid(7 * 24 * 3_600 * 1_000_000)
}

/// What the transport measured about one request, for Resource Timing. Times are
/// [`process_ms`] readings, 0 for a phase that did not happen (no redirect).
///
/// The connection phases (`domainLookup*`, `connect*`, `secureConnectionStart`) are those of the connection
/// the final hop OPENED, as Chromium's connect jobs stamp them (`net::LoadTimingInfo::ConnectTiming`); all 0 when
/// it rode one already open (`socket_reused`), which the entries then report as `fetchStart`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchTiming {
    /// Every URL the request went to, the request's own first (Fetch's URL list).
    pub url_list: Vec<String>,
    /// When the caller asked.
    pub start: f64,
    pub redirect_start: f64,
    pub redirect_end: f64,
    /// When the final hop began (`start` without a redirect).
    pub fetch_start: f64,
    /// Just before the final hop's request was written.
    pub request_start: f64,
    /// When the final response's headers arrived.
    pub response_start: f64,
    /// When the body had been read to the end.
    pub response_end: f64,
    /// ALPN-style protocol of the final response: `http/1.0`, `http/1.1`, `h2`, `h3`.
    pub protocol: String,
    /// Body bytes after the content coding is removed (`decodedBodySize`).
    pub body_size: u64,
    /// Body bytes as they came over the wire, still content-coded (`encodedBodySize`; `transferSize` is
    /// this plus 300). Equal to `body_size` for a body with no coding.
    #[serde(default)]
    pub encoded_size: u64,
    pub status: u16,
    /// The final response's `Content-Type` essence (`text/plain`).
    pub content_type: String,
    /// The final response's `Timing-Allow-Origin` values.
    pub timing_allow_origin: Vec<String>,
    /// The final response's `Server-Timing` fields, combined (`a;dur=1, b`): the entry's `serverTiming`
    /// (parsed by korvus-webapi's `core/timing/server_timing.rs`, as Blink parses it).
    #[serde(default)]
    pub server_timing: String,
    /// How many times the request went to the network again before its final response: each redirect it
    /// followed, and a navigation's `Critical-CH` restart, which Chrome counts as one.
    #[serde(default)]
    pub redirect_count: u32,
    /// When a navigation restarted for `Critical-CH` (a [`process_ms`] reading; 0 = it did not): the
    /// entry's `criticalCHRestart`.
    #[serde(default)]
    pub critical_ch_restart: f64,
    /// When the final hop's connection began looking up its host (a [`process_ms`] reading; 0 = no lookup: a
    /// connection through a proxy, whose lookups are the proxy's, or one it did not open): `domainLookupStart`.
    #[serde(default)]
    pub domain_lookup_start: f64,
    /// When that lookup ended: `domainLookupEnd`.
    #[serde(default)]
    pub domain_lookup_end: f64,
    /// When the connection's connect began -- its lookup's end on a direct connection, the connect job's start
    /// behind a proxy (proxy lookup, TCP and tunnel included); 0 = it did not open one: `connectStart`.
    #[serde(default)]
    pub connect_start: f64,
    /// When the TLS handshake with the destination began (0 = none, or a connection it did not open):
    /// `secureConnectionStart`.
    #[serde(default)]
    pub ssl_start: f64,
    /// When the connection the final hop OPENED became usable (TCP, proxy tunnel and TLS done; a [`process_ms`]
    /// reading; 0 = it rode one already open): the entry's `connectEnd`.
    #[serde(default)]
    pub connect_end: f64,
    /// Where the answer came from (Blink's `CacheState`): `""` the network, `"local"` a stored answer
    /// served as it was (`transferSize` 0), `"validated"` a stored answer a `304` confirmed (`transferSize` 300);
    /// either of the last two is `deliveryType` `"cache"`.
    #[serde(default)]
    pub cache: String,
}

/// The COOP/COEP a document was DELIVERED with -- the cross-origin-isolation facts of the current
/// document, resolved once per navigation from its response headers.
///
/// A browsing context's cross-origin isolation is not a property of its markup: it is carried on the
/// navigation RESPONSE, in `Cross-Origin-Opener-Policy` and `Cross-Origin-Embedder-Policy`. The
/// parser and the resolution algorithm live in `korvus-webapi` (`core/html/isolation_policy.rs`);
/// the RESULT lives here so that a table body reads it as a service (`ctx.get::<IsolationPolicyHandle>()`)
/// and never re-parses a header, and so that the runtime -- which owns the navigation and the
/// response -- is the one that provides it (invariant I9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveredIsolation {
    /// `Cross-Origin-Opener-Policy`: `unsafe-none`, `same-origin`, `same-origin-allow-popups` or
    /// `noopener-allow-popups`; anything else -- a parse failure, an obsolete token -- is `unsafe-none`.
    pub coop: String,
    pub coop_report_only: String,
    /// `Cross-Origin-Embedder-Policy`: `unsafe-none`, `require-corp` or `credentialless`.
    pub coep: String,
    pub coep_report_only: String,
    /// The document's origin, ASCII-serialized (`"null"` for an opaque one).
    pub origin: String,
    /// Delivered in a secure context. COOP and COEP are ignored outside one -- an insecure document's
    /// four values are `unsafe-none` whatever its headers said.
    pub secure: bool,
}

impl DeliveredIsolation {
    /// The unsafe-none default: what a document with no policy headers, an insecure document, or a
    /// synthesised document has.
    pub fn unsafe_none(origin: String, secure: bool) -> DeliveredIsolation {
        DeliveredIsolation {
            coop: "unsafe-none".to_string(),
            coop_report_only: "unsafe-none".to_string(),
            coep: "unsafe-none".to_string(),
            coep_report_only: "unsafe-none".to_string(),
            origin,
            secure,
        }
    }

    /// Cross-origin isolation is granted when a document's OWN COOP is `same-origin` and its OWN COEP is
    /// `require-corp` or `credentialless` -- both read off the response, both secure-context-gated.
    pub fn is_cross_origin_isolated(&self) -> bool {
        self.secure && self.coop == "same-origin" && (self.coep == "require-corp" || self.coep == "credentialless")
    }
}

impl Default for DeliveredIsolation {
    fn default() -> Self {
        DeliveredIsolation::unsafe_none("null".to_string(), false)
    }
}

/// The isolation-policy **service** on the kernel [`kernel::Context`]: what `crossOriginIsolated`
/// answers, and the delivered COOP/COEP of the current document (which a popup's first real navigation
/// compares its own response against). Provided at every navigation from the response headers; absent
/// until the first one, which readers treat as the unsafe-none default.
pub trait IsolationPolicy: Send + Sync {
    /// `window.crossOriginIsolated`.
    fn cross_origin_isolated(&self) -> bool;
    /// The policy the current document was delivered with.
    fn delivered(&self) -> DeliveredIsolation;
}

impl IsolationPolicy for DeliveredIsolation {
    fn cross_origin_isolated(&self) -> bool {
        self.is_cross_origin_isolated()
    }
    fn delivered(&self) -> DeliveredIsolation {
        self.clone()
    }
}

/// The isolation-policy service handle -- `Arc` so the `Context` shares it; a contract type (no engine
/// deps), the same shape as [`PersonaHandle`] and [`RenderHandle`].
pub struct IsolationPolicyHandle(pub std::sync::Arc<dyn IsolationPolicy>);

/// A **serving plugin** — the host-level layer (above the per-engine [`kernel::Context`]) that exposes
/// the core over a socket (CDP, MCP, …). This is D5's "serving is a plugin": the kernel is an embeddable
/// library, and loading a serving plugin turns it into a daemon (omit it → library mode). The engine
/// factory is the plugin's own concern, so this trait stays std-only (korvus-kernel references no engine).
/// The future is intentionally not `Send` — the host serves single-threaded (an Engine holds a `!Send`
/// V8 isolate).
pub trait ServingPlugin {
    fn name(&self) -> &str;
    /// Bind `addr` and serve until shutdown (owns the accept loop). Consumes the plugin.
    fn serve(
        self: Box<Self>,
        addr: std::net::SocketAddr,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>>>>;
}

// ---- Behavior: human-like input generation ----

/// One cursor sample: integer viewport coordinates (real DOM `clientX/Y` are integers) + a relative
/// microsecond timestamp. A move is a `Vec<MotionSample>`; `engine` expands these into trusted events.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionSample {
    pub x: i32,
    pub y: i32,
    /// Microseconds since the start of this move (behavior owns the clock; the scheduler offsets it).
    pub t_us: f64,
}

/// A floating-point point in the abstract viewport (before integer quantization).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}
impl Point {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// The stable per-session motor persona (L0). Seeds the generator so a session is internally consistent
/// but distinct across sessions (defeats cross-session correlation + replay-match). Derived from the
/// device persona (device-class → poll rate, etc.); never invented per-detector.
#[derive(Debug, Clone, Copy)]
pub struct MotorProfile {
    /// Seeds the generator's PRNG (distinct per session).
    pub seed: u64,
    /// Pointer sample rate in Hz (125 default USB; 500/1000 gaming). Governs inter-sample Δt.
    pub poll_hz: f64,
    /// 8–12 Hz physiological tremor amplitude in pixels (sub-pixel; manifests as ±1px dither).
    pub tremor_amp: f64,
    /// Tremor frequency in Hz (8–12).
    pub tremor_hz: f64,
    /// Overshoot tendency 0..1 (WindMouse produces overshoot + correction near the target).
    pub overshoot: f64,
}
impl Default for MotorProfile {
    fn default() -> Self {
        Self { seed: 1, poll_hz: 125.0, tremor_amp: 0.6, tremor_hz: 10.0, overshoot: 0.5 }
    }
}

/// The swappable trajectory model (L3): neuromotor (primary, self-contained) | recorded-replay |
/// learned. `behavior` implements `NeuromotorSource`; this is the seam (mirrors `RenderBackend`).
pub trait TrajectorySource {
    /// Plan a pointer move from `from` to `to`, aiming inside a target of width `target_w` (Fitts),
    /// returning the full sample stream (integer coords, µs timestamps). Fresh stochasticity per call
    /// (anti-replay): successive calls on the same source never repeat.
    fn plan_move(&mut self, from: Point, to: Point, target_w: f64) -> Vec<MotionSample>;
}

/// The PRIORITY fields a browser writes into its HTTP/2 HEADERS frame — `weight` in the RFC 9113
/// §5.3.2 range 1..=256, not the wire byte, which is one less.
///
/// A fingerprint field like the ClientHello's, and one an emulation preset can get wrong on its own:
/// measured 2026-09-18 off a loopback TLS/h2 sink, Chrome 150, Brave 151, Edge 151 and Opera 135 all
/// open a stream with `exclusive`, dependency 0 and weight **256**, while every `chrome_*`/`opera_*`
/// preset the pinned `wreq-util` ships writes **220**. Same browser family, same page, one constant
/// apart on every request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct H2Priority {
    /// The stream this one depends on. 0 = the connection root, which is what the family sends.
    pub dep_stream: u32,
    /// 1..=256. Serialised as `weight - 1` in the frame.
    pub weight: u16,
    /// The E bit.
    pub exclusive: bool,
}

/// A network fingerprint selector — resolved by the `transport` crate into real handshake bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct TlsProfile {
    /// e.g. "chrome_148"; maps to a `wreq_util::Profile` in the transport engine.
    pub id: String,
    /// The OS the wire should claim: `macos` · `windows` · `linux` · `android` · `ios`.
    ///
    /// A SEPARATE AXIS FROM THE PRESET, and it has to be, because the emulation library treats it as
    /// one: the preset decides the ClientHello and the HTTP/2 settings, the platform decides the
    /// default `sec-ch-ua-platform` and its neighbours. Leaving it implicit lets the headers on the
    /// wire disagree with the UA in the same request.
    pub platform: String,
    /// What this device writes into the PRIORITY fields of its HTTP/2 HEADERS frame, when it has
    /// been MEASURED off a socket. `None` means "not measured": the emulation preset's own value
    /// goes out, which is the behaviour every bundle had before it was measured.
    ///
    /// A SEPARATE AXIS FROM THE PRESET, for the same reason `platform` is. The preset is a whole
    /// browser's wire as one library recorded it; this is one field of it that the library's
    /// recording and the shipping browser disagree on, and the bundle is where a measured
    /// disagreement with the preset is stated (its `known_incoherence "tls-version"` states the
    /// other one).
    pub h2_priority: Option<H2Priority>,
    /// The HTTP/3 (QUIC) preset this device speaks, when it has been MEASURED off a QUIC sink (H3, 2026-10-11):
    /// e.g. `chrome_150`, which the transport resolves into the whole QUIC + HTTP/3 wire. `None` means "never
    /// measured": the persona speaks no HTTP/3 at all, which is what every bundle did before.
    pub quic: Option<String>,
    /// The `trust_anchors` extension (0xca34) this device's ClientHello carries, as MEASURED off a TLS sink: the
    /// extension body in hex (its two-byte list length, then the 8-bit length-prefixed trust anchor IDs). Chrome 152
    /// sends one on TCP and QUIC alike; `None` = the ClientHello carries none, as the emulation presets do.
    pub trust_anchors: Option<String>,
}

/// A persona's per-origin farbling policy: the supports Brave redraws each scalar from,
/// per (session, eTLD+1 host), as captured in `bundles/*/farbling.json`. The engine reads these; it
/// does not know them (I4). An absent member ([`None`]/empty) means that scalar is served static.
#[derive(Debug, Clone, Default)]
pub struct FarblingPolicy {
    /// `navigator.hardwareConcurrency`: drawn uniformly from `[2, max]`. `max` is the host's unfarbled
    /// core count as the capture read it (12 here). `None` = serve the static value.
    pub hardware_concurrency_max: Option<u32>,
    /// `navigator.deviceMemory`: drawn uniformly from this set ({4,8,16,32} for Brave here). Empty =
    /// serve the static value.
    pub device_memory_set: Vec<f64>,
}

/// The navigator-visible scalars of a persona.
#[derive(Debug, Clone, Default)]
pub struct NavigatorProfile {
    pub user_agent: String,
    pub platform: String,
    pub hardware_concurrency: u32,
    pub device_memory: f64,
    pub languages: Vec<String>,
    /// **The `Accept-Language` header this device puts on the wire**, verbatim, as the capture read
    /// it back off its own request — the wire half of [`languages`](Self::languages).
    ///
    /// CAPTURED, NOT DERIVED, and the two are not the same function in every browser. Measured
    /// 2026-09-18 against the four installed Chromium-family browsers on this box, all at the same
    /// `ko_KR.UTF-8` locale, by reading the header off a loopback socket:
    ///
    /// ```text
    /// browser        navigator.languages              Accept-Language
    /// Chrome 150     ko-KR,ko,en-US,en                ko-KR,ko;q=0.9,en-US;q=0.8,en;q=0.7
    /// Edge 151       ko,en,en-US                      ko,en;q=0.9,en-US;q=0.8
    /// Opera 135      ko-KR,ko,en-US,en                ko-KR,ko;q=0.9,en-US;q=0.8,en;q=0.7
    /// Brave 151      ko-KR                            ko-KR,ko;q=0.N        (N random per session)
    /// ```
    ///
    /// Three of the four are `net::HttpUtil::GenerateAcceptLanguageHeader` over the same list, so a
    /// rule derived from Chrome alone reproduces them — and gets Brave wrong twice over, because
    /// Brave REDUCES the list to its first language and then farbles the q-value. Deriving would put
    /// a browser-family branch in the engine (P2 forbids it) and would still be a guess for the next
    /// browser. So the header is a recorded value like the UA, and an engine that has none sends
    /// none rather than inventing one.
    ///
    /// An empty string means the capture did not record it. It is NOT a licence to fall back to the
    /// emulation preset's own `en-US,en;q=0.9`: that is the value this field exists to stop, and it
    /// contradicted `navigator.languages` in the same page load.
    pub accept_language: String,
    /// `navigator.globalPrivacyControl`, and therefore whether this device sends `Sec-GPC: 1`.
    ///
    /// `None` = the device does not expose the member at all, which is the Chrome / Edge / Opera
    /// answer; `Some(true)` is Brave's. It is one field because it is one claim: a browser that
    /// answers `true` in JS and sends no header — or sends the header while JS says the member does
    /// not exist — is contradicting itself in the one place a server can check both halves.
    pub global_privacy_control: Option<bool>,
    /// The `Accept` the captured browser puts on a navigation (Brave drops Chromium's
    /// `application/signed-exchange;v=b3;q=0.7`, measured 2026-09-24). Every shipped bundle names its own;
    /// `None` makes the transport fall back to the Fetch Standard's default, which names no browser.
    pub accept_navigation: Option<String>,
    /// **This browser blocks third-party cookies**. A cross-site context's credentialed
    /// requests say so in `Sec-Fetch-Storage-Access`: measured 2026-10-02 on a localhost frame under a
    /// 127.0.0.1 page, stock Brave 151 sends `none` (and `inactive` with an `Origin` for the request back
    /// to the top-level site), stock Chrome 150, Edge 151 and Opera 135 send `active`. Recorded per
    /// bundle like `accept_navigation`, so no browser-family branch decides it.
    pub third_party_cookies_blocked: bool,
    /// `navigator.keyboard.getLayoutMap()`'s entries, in the order the browser's map iterates them
    /// (Blink's `HashMap` order -- only a measurement can give it). `None` = `navigator.keyboard` is
    /// `null`, Brave 151's answer; Chrome 150, Edge 151 and Opera 135 on a US-layout desktop give the
    /// same 48 entries (measured 2026-10-08).
    pub keyboard_layout: Option<Vec<(String, String)>>,
}

/// `performance.memory` — Chrome's non-standard heap window, and a fingerprint value like any other.
///
/// Captured, never computed: the numbers describe the machine the profile was taken on, and an
/// engine that answers with its own V8 heap would report a heap no Chrome tab has. `0` means the
/// profile did not capture them, and the member is then not exposed at all rather than exposed with
/// invented numbers.
#[derive(Debug, Clone, Default)]
pub struct MemoryProfile {
    pub js_heap_size_limit: f64,
    pub total_js_heap_size: f64,
    pub used_js_heap_size: f64,
    /// The heap of an EMPTY document in a fresh renderer of this browser, `(total, used)` as `performance.memory`
    /// reads them in the document's first script (`None`: not measured) -- where a reading starts before the page has
    /// allocated anything.
    pub empty_document: Option<(f64, f64)>,
}

/// The screen-visible scalars of a persona. `avail_*` describe the desktop work area inside the
/// screen (the OS menu bar / taskbar / dock inset). On a real Mac the menu bar makes
/// `avail_height < height` and `avail_top > 0`; a headless screen reporting `avail == full` with
/// `avail_top == 0` is a coherence tell. `avail_width`/`avail_height` of 0 mean "not captured" and
/// fall back to `width`/`height` (see the engine's `JsScreen`).
#[derive(Debug, Clone, Default)]
pub struct ScreenProfile {
    pub width: u32,
    pub height: u32,
    pub color_depth: u32,
    pub device_pixel_ratio: f64,
    pub avail_width: u32,
    pub avail_height: u32,
    pub avail_top: i32,
    pub avail_left: i32,
    /// The browser WINDOW's own geometry, as captured. Every one of these is a fingerprint value, so
    /// none of them may be a constant in the engine -- they belong to the profile, like the screen
    /// scalars above.
    ///
    /// `0` means "not captured" and falls back through the work area (see the engine's
    /// `window_geometry`). The fallback exists because these were added after the current capture was
    /// taken; a fresh capture should fill them, and until it does the engine models a maximized
    /// window rather than inventing a chrome inset.
    ///
    /// Why they cannot come from `width`/`height`: a window is not the screen. On the captured Mac
    /// the menu bar makes `avail_height` 98px shorter than `height` and puts `avail_top` at 33, so a
    /// window reporting `innerHeight == height` and `screenY == 0` is claiming to be taller than the
    /// desktop work area AND positioned underneath the menu bar. Both are impossible, and either is
    /// one subtraction away from being caught.
    pub window_inner_width: u32,
    pub window_inner_height: u32,
    pub window_outer_width: u32,
    pub window_outer_height: u32,
    pub window_screen_x: i32,
    pub window_screen_y: i32,
    /// `screen.orientation` -- the captured `ScreenOrientation`'s `type` and `angle`.
    ///
    /// A FINGERPRINT VALUE LIKE EVERY OTHER FIELD OF THIS STRUCT, so it is the profile's and never a
    /// constant in the engine: four desktop Linux captures all say `landscape-primary` / 0 today, and
    /// the next capture is a phone that says `portrait-primary` / 0 or `landscape-secondary` / 180.
    ///
    /// AN EMPTY `orientation_type` MEANS "NOT CAPTURED", and unlike `avail_*` it does NOT fall back:
    /// there is no other field to compute an orientation from (`width > height` is a guess about the
    /// device, not a reading of it), so the engine exposes no orientation object at all rather than
    /// inventing one. That is the same rule [`MemoryProfile`] states for an uncaptured heap -- absent
    /// beats fabricated, because a fabricated value is a claim about a device nobody measured.
    pub orientation_type: String,
    pub orientation_angle: u32,
    /// The display's refresh rate in Hz -- the cadence `requestAnimationFrame` runs at (a vsync every
    /// `1000 / refresh_rate` ms). Measured, not assumed: Chrome 150 on the captured desktop runs rAF
    /// every 16.7 ms. `0` = not captured; the engine then uses the common 60 Hz.
    pub refresh_rate: f64,
}

/// Navigator scalars that are structurally constant within a browser family. Defaults are the
/// Chrome-desktop constants (identical across every real Chrome), so a Chrome persona need not set
/// them; a non-Chrome persona overrides. Not per-device values — see [`Persona::navigator_extra`].
#[derive(Debug, Clone, serde::Serialize)]
pub struct NavigatorExtra {
    pub vendor: String,
    pub vendor_sub: String,
    pub app_name: String,
    pub app_code_name: String,
    pub product: String,
    pub product_sub: String,
    pub max_touch_points: u32,
    pub cookie_enabled: bool,
    pub pdf_viewer_enabled: bool,
    /// `navigator.doNotTrack` — `None` serializes to JS `null` (Chrome's default when DNT is off).
    pub do_not_track: Option<String>,
    /// `navigator.cpuPerformance` (Chrome 152, `navigator_cpu_performance.cc`): the platform's CPU performance
    /// tier -- 1 low, 2 mid, 3 high, 4 ultra, 0 unknown. A device fact, so it comes from the capture.
    pub cpu_performance: u16,
}

impl Default for NavigatorExtra {
    fn default() -> Self {
        // Chrome-desktop constants; identical across every real Chrome install (a Gecko/WebKit
        // persona would override product/productSub/vendor). These are structural, not a device id.
        Self {
            vendor: "Google Inc.".to_string(),
            vendor_sub: String::new(),
            app_name: "Netscape".to_string(),
            app_code_name: "Mozilla".to_string(),
            product: "Gecko".to_string(),
            product_sub: "20030107".to_string(),
            max_touch_points: 0,
            cookie_enabled: true,
            pdf_viewer_enabled: true,
            do_not_track: None,
            cpu_performance: 0,
        }
    }
}

/// One `{brand, version}` entry in a UA Client-Hints brand list.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Brand {
    pub brand: String,
    pub version: String,
}

/// One `navigator.plugins` entry. Real Chrome ships five fixed PDF-viewer entries; an empty list is
/// a classic headless tell.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct PluginInfo {
    pub name: String,
    pub description: String,
    pub filename: String,
}

/// `navigator.connection` (NetworkInformation) scalars.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub effective_type: String,
    pub rtt: u32,
    pub downlink: f64,
    pub save_data: bool,
}

impl Default for Connection {
    fn default() -> Self {
        Self { effective_type: "4g".to_string(), rtt: 100, downlink: 10.0, save_data: false }
    }
}

/// The UA Client-Hints (low + high entropy) plus the plugin/connection/timezone surface. Every field
/// is per-device and comes from the captured profile — routing these through the persona is what keeps
/// `navigator.userAgentData`, `navigator.plugins`, and `navigator.connection` coherent with the UA
/// header and the TLS/H2 fingerprint (a mismatch is an instant "Frankenstein" tell).
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientHints {
    /// Low-entropy brand list (`navigator.userAgentData.brands`), GREASE brand included.
    pub brands: Vec<Brand>,
    /// High-entropy brand list with full version strings (`getHighEntropyValues(["fullVersionList"])`).
    pub full_version_list: Vec<Brand>,
    pub mobile: bool,
    pub platform: String,
    pub architecture: String,
    pub bitness: String,
    pub model: String,
    pub platform_version: String,
    pub ua_full_version: String,
    pub wow64: bool,
    /// `formFactors` (UA-CH), e.g. `["Desktop"]`.
    pub form_factors: Vec<String>,
    /// `navigator.permissions.query()` states, by permission name, from the persona's source browser.
    pub permissions: Vec<(String, String)>,
    /// `navigator.storage.estimate()`'s `(quota, usage)` from the persona's source browser.
    pub storage_quota: Option<(f64, f64)>,
    /// The `kind` of each device an unprompted `navigator.mediaDevices.enumerateDevices()` listed on
    /// the persona's source browser, in its order. A fact about the capture MACHINE, not a Chromium
    /// constant: Brave 151 on the Linux capture host lists `audiooutput` alone.
    pub media_device_kinds: Vec<String>,
    /// `navigator.mediaDevices.getSupportedConstraints()`'s members (each `true`) on the persona's
    /// source browser, in the order it returns them. The set grows with the build: Chrome 150 and
    /// Brave 151 report 36.
    pub supported_constraints: Vec<String>,
    /// What the built-in AI APIs' `availability()` answered with no options on the persona's source browser, by
    /// interface (`Summarizer`, `LanguageModel`, `LanguageDetector`, `Translator` with a language pair): an
    /// `Availability` value, `"pending"` (it never settled), or `"reject:<name>:<message>"`. A fact about the
    /// capture machine and its model downloads, not a constant: stock Chrome 150 on the Linux capture host answers
    /// `"unavailable"` for all four, Edge 151 `"downloadable"` for `LanguageDetector`.
    pub ai_availability: Vec<(String, String)>,
    pub plugins: Vec<PluginInfo>,
    pub connection: Connection,
    /// IANA timezone id (`Intl.DateTimeFormat().resolvedOptions().timeZone`), for coherence with the
    /// exit IP; carried here so a future Intl override + the coherence test can read it.
    pub timezone: String,
    /// ICU's DEFAULT LOCALE as the capture recorded it — `Intl.DateTimeFormat().resolvedOptions().locale`
    /// read with no argument, which is the browser's UI language and **not** `navigator.language`.
    ///
    /// The two are different facts and the bundles prove it rather than argue it (measured
    /// 2026-09-18 on the four captures in this tree): brave/chrome/opera record `intl.locale = "ko"`
    /// against `languages[0] = "ko-KR"`, and **edge records the opposite pair** — `intl.locale =
    /// "ko-KR"` against `language = "ko"`. No rule turns one into the other, which is why this is
    /// carried and not derived. Real Brave 151 on this box answers `"ko"` to a default
    /// `Intl.DateTimeFormat()`; korvus answered `"ko-KR"`, because `Engine::assemble` set ICU's locale
    /// from `navigator.languages[0]` — the nearest value to hand, and the wrong one.
    pub intl_locale: String,
}

/// Below-the-language observer hook. The default is [`NoopObserver`]; an RE plugin implements this
/// inside `engine` against an instrumented V8. Host code never changes when it's enabled.
pub trait EngineObserver: Send + Sync {
    fn on_property_read(&self, _key: &str) {}
}

/// The zero-overhead default observer.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopObserver;
impl EngineObserver for NoopObserver {}

/// Launch configuration assembled by the `korvus` binary and validated by the Coherence Engine.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub persona: String,
    pub render: String,
    pub transport: String,
}

// -- Remote frames: a cross-site `<iframe>` in its own engine on its own thread ----------

/// What a parent hands the child engine of a REMOTE frame: the frame's committed response and the
/// facts about its embedding that a top-level document cannot know about itself.
#[derive(Debug, Clone, Default)]
pub struct RemoteFrameSpawn {
    /// The response's final URL (the frame document's URL).
    pub url: String,
    /// The response headers, as delivered.
    pub headers: Vec<(String, String)>,
    /// The response body.
    pub body: Vec<u8>,
    /// The origin of the document holding the `<iframe>`: what the child's `parent` is, and the
    /// `origin` of every message the parent sends it.
    pub parent_origin: String,
    /// The top-level document's URL.
    pub top_url: String,
    /// The frame document's `document.referrer`, under the embedder's referrer policy.
    pub referrer: String,
    /// The frame's viewport -- the `<iframe>`'s content box -- in CSS px.
    pub viewport: (f64, f64),
    /// The `<iframe>`'s `scrolling` turns its viewport's scrollbars off (`no`, `off`, `noscroll`).
    pub scrollbars_off: bool,
    /// Scripts run in the child document before its own (diagnosis hooks: `KORVUS_FRAME_EARLY`).
    pub injected: Vec<String>,
    /// `location.ancestorOrigins`: the parent's origin first, the top's last.
    pub ancestor_origins: Vec<String>,
    /// The policy-controlled features the frame INHERITS disabled (Permissions Policy sec. 9.7, decided by
    /// the parent, which holds the container and its declared policy).
    pub disabled_features: Vec<String>,
    /// The browsing session's farbling seed: one session, so the child farbles as the page does.
    pub session_seed: Option<u64>,
    /// The frame navigation's own timing, for its `PerformanceNavigationTiming` and `timeOrigin`.
    pub timing: Option<FetchTiming>,
    /// Whether the frame's agent cluster is cross-origin isolated -- the TOP document's isolation, which a frame's
    /// own response (its COOP is ignored) cannot decide.
    pub cluster_isolated: bool,
}

/// What a remote frame tells its parent.
#[derive(Debug, Clone)]
pub enum RemoteFrameEvent {
    /// `parent.postMessage(...)`: the message as V8 structured-clone bytes, and the frame's origin.
    Message { data: Vec<u8>, origin: String },
    /// The frame document's `load` event fired.
    Loaded,
}

/// The next event of one remote frame; `None` once the frame is gone.
pub type RemoteFrameFuture = std::pin::Pin<Box<dyn std::future::Future<Output = Option<RemoteFrameEvent>>>>;

/// The host that runs remote frames -- provided by the kernel (`korvus-runtime`, which can build an
/// `Engine`), consumed by `core/html/browsing_context.rs` in `korvus-webapi`.
pub trait RemoteFrameHost: Send + Sync {
    /// Start a child engine for `spawn`; answers its id at once (the child boots on its own thread).
    fn spawn(&self, spawn: RemoteFrameSpawn) -> u64;
    /// The parent's `postMessage` into frame `id`: structured-clone bytes and the parent's origin.
    fn post(&self, id: u64, data: Vec<u8>, origin: String);
    /// The next event from frame `id`.
    fn recv(&self, id: u64) -> RemoteFrameFuture;
    /// Stop frame `id`'s engine.
    fn terminate(&self, id: u64);
    /// How many remote frames are running.
    fn live(&self) -> usize;
}

/// The remote-frame service handle on the kernel `Context`.
pub struct RemoteFrames(pub std::sync::Arc<dyn RemoteFrameHost>);

/// One `SpeechSynthesisVoice` (Web Speech API), as the persona's browser lists it.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default)]
pub struct SpeechVoice {
    pub name: String,
    pub lang: String,
    #[serde(rename = "voiceURI")]
    pub voice_uri: String,
    #[serde(rename = "localService")]
    pub local_service: bool,
    pub default: bool,
}

/// A device's `getCapabilities` answers ([`Persona::webrtc_capabilities`]): for each of `RTCRtpReceiver.audio`,
/// `RTCRtpReceiver.video`, `RTCRtpSender.audio` and `RTCRtpSender.video`, the `RTCRtpCapabilities` dictionary as
/// JSON text, its members in the order a page reads them (a WebIDL dictionary's: lexicographic).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebrtcCapabilities {
    pub by_side_kind: std::collections::BTreeMap<String, String>,
}

/// A device's offer for receive-only media ([`Persona::webrtc_offer`]): its `m=audio` and `m=video`
/// sections, each exactly as the device wrote it, lines joined by CRLF.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebrtcOffer {
    pub audio: String,
    pub video: String,
}

// -- The element-induced subresource load ---------------------------------------------

/// One element-induced request: what `img.src`, `<script src>`, `<link rel=icon href>`, `<iframe src>`
/// and a browsing context's own navigation ask the network for.
///
/// The element/attribute -> destination decision is the WEB API's (it read the tag), so it arrives
/// already made. Everything the request still needs and only the kernel knows -- the document base a
/// relative URL resolves against, the worn identity's headers, which transport carries it -- is the
/// loader's, which is why this is a service and not a function.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SubresourceRequest {
    /// The URL exactly as the element carries it: absolute, or relative to the document base.
    pub url: String,
    /// Fetch's request destination token, the one `Sec-Fetch-Dest` serializes ("image", "script",
    /// "style", "iframe", "document", "" for the empty destination, ...).
    pub destination: String,
    /// Fetch's request mode token ("no-cors", "cors", "same-origin", "navigate").
    pub mode: String,
    /// The request method, uppercase. `POST` only for a `method=post` form submission into a nested
    /// or auxiliary browsing context; every other element-induced load is a `GET`.
    #[serde(default = "get_method")]
    pub method: String,
    /// The submission body, already serialised in the form's encoding.
    #[serde(default)]
    pub body: Option<String>,
    /// The `Content-Type` that body travels under.
    #[serde(default)]
    pub content_type: Option<String>,
    /// The requesting document's SITE FOR COOKIES: `""` the page's own, `"null"` under a cross-site
    /// ancestor, else the top-level URL. The loader sends only `SameSite=None` cookies across sites.
    #[serde(default)]
    pub site_for_cookies: String,
    /// The requesting document's URL when it is not the page's own: a frame's element
    /// load is initiated by the FRAME -- its `Referer`, its `Sec-Fetch-Site`. `""` is the page.
    #[serde(default)]
    pub client_url: String,
    /// The referrer policy token the request goes out under: the element's
    /// `referrerpolicy`, else its document's. `""` is the default policy.
    #[serde(default)]
    pub referrer_policy: String,
    /// The HTTP/2 `priority` field the load goes out with, as its loader computed Blink's load priority
    /// for it: `Some("")` is no field, `None` the destination's default.
    #[serde(default)]
    pub priority: Option<String>,
    /// The credentials mode of a load its element made CORS (`crossorigin`, a module script, a font):
    /// `"same-origin"` or `"include"` (`crossorigin=use-credentials`). `""` for any other load, whose
    /// mode decides: a no-cors element load includes credentials, a `<track>`'s same-origin one too.
    #[serde(default)]
    pub credentials: String,
    /// When the NAVIGATION this request fetches was initiated -- a frame's `src` set or its element inserted, a
    /// form or link targeting it, `location` -- which HTML makes its document's time origin: before the loader
    /// got to the request (an element load is fetched on a later turn of the event loop). `None` for a
    /// subresource, and for a navigation the transport may date itself.
    #[serde(skip)]
    pub navigation_start: Option<std::time::Instant>,
    /// A FRAME's navigation only -- its parent document's encoding (WHATWG name) and origin, for the decoder's
    /// "parent's encoding" step (Blink `TextResourceDecoderBuilder`, `kEncodingFromParentFrame`): a frame document that
    /// declares no usable charset inherits it when its parent can access it (same origin). `None` for every other load,
    /// a popup's, and a sandboxed frame's (an opaque origin no parent can access).
    #[serde(default)]
    pub parent_encoding: Option<String>,
    #[serde(default)]
    pub parent_origin: Option<String>,
}

fn get_method() -> String {
    "GET".to_string()
}

/// What came back. A subset of a full Fetch response: the four things an element-induced load acts on
/// -- the status (2xx decides `load` vs `error`), the FINAL url (a redirect moved it), the headers (a
/// navigation reads COOP off them) and the body (a classic script's source, a frame's document).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SubresourceResponse {
    pub status: u16,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
    /// The transport's timing of this load, for the document's Resource Timing entry.
    #[serde(default)]
    pub timing: Option<FetchTiming>,
    /// The encoding a browsing context's DOCUMENT was decoded with (a `navigate`-mode load only), by its
    /// WHATWG name -- what the frame document's `characterSet` reports and its forms and links encode with.
    /// `None` for every other load, whose `body` is the transport's UTF-8 view.
    #[serde(default)]
    pub encoding: Option<String>,
}

/// Deliberately NOT `Send`: the transport future this ultimately awaits is not, and these loads run on
/// the isolate's own single-threaded task set. The SERVICE is `Send + Sync` (a `kernel::Service` must
/// be); the future it hands back is not.
pub type SubresourceFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<SubresourceResponse, String>>>>;

/// The subresource loader: the one path an element-induced load takes to the network.
///
/// Separate from a page `fetch()` on purpose. A page may not name its own `Sec-Fetch-Dest` -- that is
/// the one header a Fetch Metadata check exists to trust -- so the destination is only ever set by the
/// Rust call site that read the tag, and this trait is how that call site reaches the network without
/// owning an HTTP client of its own.
pub trait SubresourceLoader: Send + Sync {
    fn fetch(&self, req: SubresourceRequest) -> SubresourceFuture;
}

/// The subresource-load service handle on the kernel [`kernel::Context`] -- the same shape as
/// [`RenderHandle`]/[`PersonaHandle`]/[`CssHandle`]. Provided by the kernel (`korvus-runtime`),
/// consumed by `core/html/subresource.rs` in `korvus-webapi`.
pub struct SubresourceFetch(pub std::sync::Arc<dyn SubresourceLoader>);

/// One image response the subresource loader kept, as the fetch saw it.
#[derive(Debug, Clone)]
pub struct FetchedImage {
    pub status: u16,
    /// The FINAL url -- a redirect to another origin taints a canvas the request url would not.
    pub final_url: String,
    pub headers: Vec<(String, String)>,
    pub bytes: std::sync::Arc<Vec<u8>>,
}

/// The image bodies element-induced loads fetched, keyed by `(request url, request mode)` -- the key
/// of HTML's "list of available images" (url, CORS mode). An `<img>` load is a `SubresourceFetch`
/// whose body `core/html/subresource.rs` reads only as a status, so the pixels a later
/// `ctx.drawImage(img, ...)` paints and the CORS facts that decide whether it TAINTS the canvas have to
/// be kept where both halves can reach them: the loader that saw the response writes, the canvas
/// reads.
///
/// Bounded ([`FetchedImages::BUDGET`] bytes, oldest out first): a page of large images must not hold
/// every body for its whole life. An evicted body reads as never fetched, which `drawImage` answers
/// by drawing nothing -- the answer for an image that is not (yet) available.
#[derive(Clone, Default)]
pub struct FetchedImages(pub std::sync::Arc<std::sync::Mutex<FetchedImageList>>);

#[derive(Default)]
pub struct FetchedImageList {
    map: std::collections::HashMap<(String, String), FetchedImage>,
    order: std::collections::VecDeque<(String, String)>,
    bytes: usize,
    /// RENDER #6: bumped on every insert so the compositor (`event_loop::rendering_opportunity`) can
    /// repaint when an image finishes loading -- a loaded image's `largest-contentful-paint` otherwise
    /// never fires, because the sole compositor paint precedes the async image load. Kept monotonic
    /// across `clear()` so a reused event loop's last-painted generation never spuriously arms a frame.
    generation: u64,
}

impl FetchedImages {
    /// 128 MiB of encoded image bodies per page.
    pub const BUDGET: usize = 128 << 20;

    pub fn insert(&self, url: &str, mode: &str, img: FetchedImage) {
        let Ok(mut l) = self.0.lock() else { return };
        l.generation += 1;
        let key = (url.to_string(), mode.to_string());
        if let Some(old) = l.map.remove(&key) {
            l.bytes -= old.bytes.len();
            l.order.retain(|k| k != &key);
        }
        l.bytes += img.bytes.len();
        l.map.insert(key.clone(), img);
        l.order.push_back(key);
        while l.bytes > Self::BUDGET && l.order.len() > 1 {
            let Some(k) = l.order.pop_front() else { break };
            if let Some(old) = l.map.remove(&k) {
                l.bytes -= old.bytes.len();
            }
        }
    }

    pub fn get(&self, url: &str, mode: &str) -> Option<FetchedImage> {
        self.0.lock().ok()?.map.get(&(url.to_string(), mode.to_string())).cloned()
    }

    /// RENDER #6: the number of inserts so far. The compositor repaints when this advances past the
    /// generation it last painted at, so a freshly loaded image gets a frame (and its LCP).
    pub fn generation(&self) -> u64 {
        self.0.lock().map(|l| l.generation).unwrap_or(0)
    }

    /// Forget every body -- a navigation to a new document. `generation` is preserved (monotonic) so a
    /// reused event loop never sees it go backwards and arm a spurious frame.
    pub fn clear(&self) {
        if let Ok(mut l) = self.0.lock() {
            let g = l.generation;
            *l = FetchedImageList::default();
            l.generation = g;
        }
    }
}

// ── The SHAPE of a DOMHighResTimeStamp ───────────────────────────────────────────────
//
// Every assertion below is STRUCTURAL. Not one of them pins a timestamp, a grid or an uptime: the
// grid is read back off the clock the test just built, and what is asserted is the relationship
// between a reading and that grid — which is the thing a sensor can see.
#[cfg(test)]
mod perf_clock_shape {
    use super::*;

    /// The ULP of `x` as an f64 — the spacing of the grid a double of that magnitude lives on.
    fn ulp(x: f64) -> f64 {
        let e = ((x.abs().to_bits() >> 52) & 0x7ff) as i64 - 1023;
        (2.0f64).powi((e - 52) as i32)
    }

    /// A clock whose origin sits at a chosen absolute monotonic microsecond, so the test can walk the
    /// tick ladder by hand instead of racing a real one.
    fn clock_at(start_abs_us: i64) -> PerfClock {
        PerfClock {
            start: std::time::Instant::now(),
            start_abs_us,
            origin_wall_us: 0,
            secret: TEST_SECRET,
            resolution_us: COARSE_RESOLUTION_US,
            #[cfg(feature = "analysis")]
            rec0_ns: 0,
        }
    }

    /// The secret the shape tests clamp with (the first of the reference vectors' three).
    const TEST_SECRET: u64 = 0x0123_4567_89ab_cdef;

    /// A cross-origin-isolated realm clamps to 5 µs, on the same arithmetic; leaving
    /// isolation returns to the 100 µs tick.
    #[test]
    fn an_isolated_clock_clamps_to_five_microseconds() {
        let mut c = clock_at(100_858_800_000_000);
        c.set_cross_origin_isolated(true);
        let g = ulp(c.start_abs_us as f64 / 1000.0);
        for raw in [0i64, 4, 5, 9, 99, 100, 20_103] {
            let v = c.dom_hrt(raw);
            let tick = (raw - raw.rem_euclid(FINE_RESOLUTION_US)) as f64 / 1000.0;
            let up = tick + FINE_RESOLUTION_US as f64 / 1000.0;
            assert!((v - tick).abs() <= g || (raw % FINE_RESOLUTION_US != 0 && (v - up).abs() <= g), "{raw} -> {v:?} vs {tick} or {up}");
        }
        c.set_cross_origin_isolated(false);
        let v = c.dom_hrt(99);
        assert!(v.abs() <= g || (v - 0.1).abs() <= g, "99 µs -> {v:?}");
    }

    /// The reading is the 100 µs tick or the next, and never anything finer: the quantum survives the arithmetic,
    /// and a time ON a tick stays there (Blink rounds up only past a threshold inside the tick).
    #[test]
    fn the_quantum_is_a_hundred_microseconds_and_the_clamp_is_blinks() {
        let c = clock_at(100_858_800_000_000);
        let g = ulp(c.start_abs_us as f64 / 1000.0);
        for raw in [0i64, 1, 99, 100, 199, 20_099, 20_100] {
            let v = c.dom_hrt(raw);
            let tick = (raw - raw.rem_euclid(COARSE_RESOLUTION_US)) as f64 / 1000.0;
            let up = tick + 0.1;
            if raw % COARSE_RESOLUTION_US == 0 {
                assert!((v - tick).abs() <= g, "{raw} -> {v:?} vs {tick}: a time on a tick moved");
            } else {
                assert!((v - tick).abs() <= g || (v - up).abs() <= g, "{raw} -> {v:?} vs {tick} or {up}");
            }
        }
    }

    /// The port IS Blink's clamper: chromium-150's `time_clamper.cc`, compiled unmodified
    /// (`tools/main.cc`), gives these for three fixed secrets -- small, large, negative, both resolutions, across
    /// the ten-lower-digit boundary.
    #[test]
    fn the_clamp_is_blinks_time_clamper_exactly() {
        #[rustfmt::skip]
        const VECTORS: &[(u64, i64, bool, i64)] = &[
    (0x0123456789abcdef, 0, false, 0),
    (0x0123456789abcdef, 0, true, 0),
    (0x0123456789abcdef, 1, false, 0),
    (0x0123456789abcdef, 1, true, 0),
    (0x0123456789abcdef, 4, false, 0),
    (0x0123456789abcdef, 4, true, 5),
    (0x0123456789abcdef, 5, false, 0),
    (0x0123456789abcdef, 5, true, 5),
    (0x0123456789abcdef, 30, false, 0),
    (0x0123456789abcdef, 30, true, 30),
    (0x0123456789abcdef, 49, false, 0),
    (0x0123456789abcdef, 49, true, 50),
    (0x0123456789abcdef, 50, false, 0),
    (0x0123456789abcdef, 50, true, 50),
    (0x0123456789abcdef, 51, false, 0),
    (0x0123456789abcdef, 51, true, 55),
    (0x0123456789abcdef, 99, false, 100),
    (0x0123456789abcdef, 99, true, 100),
    (0x0123456789abcdef, 100, false, 100),
    (0x0123456789abcdef, 100, true, 100),
    (0x0123456789abcdef, 101, false, 100),
    (0x0123456789abcdef, 101, true, 100),
    (0x0123456789abcdef, 150, false, 200),
    (0x0123456789abcdef, 150, true, 150),
    (0x0123456789abcdef, 199, false, 200),
    (0x0123456789abcdef, 199, true, 200),
    (0x0123456789abcdef, 200, false, 200),
    (0x0123456789abcdef, 200, true, 200),
    (0x0123456789abcdef, 250, false, 200),
    (0x0123456789abcdef, 250, true, 250),
    (0x0123456789abcdef, 999, false, 1000),
    (0x0123456789abcdef, 999, true, 1000),
    (0x0123456789abcdef, 1000, false, 1000),
    (0x0123456789abcdef, 1000, true, 1000),
    (0x0123456789abcdef, 12345, false, 12300),
    (0x0123456789abcdef, 12345, true, 12345),
    (0x0123456789abcdef, 99999, false, 100000),
    (0x0123456789abcdef, 99999, true, 100000),
    (0x0123456789abcdef, -1, false, 0),
    (0x0123456789abcdef, -1, true, 0),
    (0x0123456789abcdef, -30, false, 0),
    (0x0123456789abcdef, -30, true, -30),
    (0x0123456789abcdef, -99, false, -100),
    (0x0123456789abcdef, -99, true, -100),
    (0x0123456789abcdef, -100, false, -100),
    (0x0123456789abcdef, -100, true, -100),
    (0x0123456789abcdef, -150, false, -200),
    (0x0123456789abcdef, -150, true, -150),
    (0x0123456789abcdef, -12345, false, -12300),
    (0x0123456789abcdef, -12345, true, -12345),
    (0x0123456789abcdef, 9999999999, false, 10000000000),
    (0x0123456789abcdef, 9999999999, true, 9999999995),
    (0x0123456789abcdef, 10000000000, false, 10000000000),
    (0x0123456789abcdef, 10000000000, true, 10000000000),
    (0x0123456789abcdef, 10000000037, false, 10000000000),
    (0x0123456789abcdef, 10000000037, true, 10000000035),
    (0x0123456789abcdef, 100858800000000, false, 100858800000000),
    (0x0123456789abcdef, 100858800000000, true, 100858800000000),
    (0x0123456789abcdef, 100858800000030, false, 100858800000100),
    (0x0123456789abcdef, 100858800000030, true, 100858800000030),
    (0x0123456789abcdef, 100858800000077, false, 100858800000100),
    (0x0123456789abcdef, 100858800000077, true, 100858800000080),
    (0x0123456789abcdef, 100858800012345, false, 100858800012400),
    (0x0123456789abcdef, 100858800012345, true, 100858800012345),
    (0x0123456789abcdef, 1789718996556837, false, 1789718996556800),
    (0x0123456789abcdef, 1789718996556837, true, 1789718996556840),
    (0x0123456789abcdef, 1789718996556899, false, 1789718996556900),
    (0x0123456789abcdef, 1789718996556899, true, 1789718996556900),
    (0x0123456789abcdef, 2600000000000051, false, 2600000000000000),
    (0x0123456789abcdef, 2600000000000051, true, 2600000000000055),
    (0xfedcba9876543210, 0, false, 0),
    (0xfedcba9876543210, 0, true, 0),
    (0xfedcba9876543210, 1, false, 0),
    (0xfedcba9876543210, 1, true, 0),
    (0xfedcba9876543210, 4, false, 0),
    (0xfedcba9876543210, 4, true, 5),
    (0xfedcba9876543210, 5, false, 0),
    (0xfedcba9876543210, 5, true, 5),
    (0xfedcba9876543210, 30, false, 0),
    (0xfedcba9876543210, 30, true, 30),
    (0xfedcba9876543210, 49, false, 0),
    (0xfedcba9876543210, 49, true, 50),
    (0xfedcba9876543210, 50, false, 0),
    (0xfedcba9876543210, 50, true, 50),
    (0xfedcba9876543210, 51, false, 0),
    (0xfedcba9876543210, 51, true, 50),
    (0xfedcba9876543210, 99, false, 100),
    (0xfedcba9876543210, 99, true, 100),
    (0xfedcba9876543210, 100, false, 100),
    (0xfedcba9876543210, 100, true, 100),
    (0xfedcba9876543210, 101, false, 100),
    (0xfedcba9876543210, 101, true, 100),
    (0xfedcba9876543210, 150, false, 100),
    (0xfedcba9876543210, 150, true, 150),
    (0xfedcba9876543210, 199, false, 200),
    (0xfedcba9876543210, 199, true, 200),
    (0xfedcba9876543210, 200, false, 200),
    (0xfedcba9876543210, 200, true, 200),
    (0xfedcba9876543210, 250, false, 300),
    (0xfedcba9876543210, 250, true, 250),
    (0xfedcba9876543210, 999, false, 1000),
    (0xfedcba9876543210, 999, true, 1000),
    (0xfedcba9876543210, 1000, false, 1000),
    (0xfedcba9876543210, 1000, true, 1000),
    (0xfedcba9876543210, 12345, false, 12300),
    (0xfedcba9876543210, 12345, true, 12345),
    (0xfedcba9876543210, 99999, false, 100000),
    (0xfedcba9876543210, 99999, true, 99995),
    (0xfedcba9876543210, -1, false, 0),
    (0xfedcba9876543210, -1, true, 0),
    (0xfedcba9876543210, -30, false, 0),
    (0xfedcba9876543210, -30, true, -30),
    (0xfedcba9876543210, -99, false, -100),
    (0xfedcba9876543210, -99, true, -100),
    (0xfedcba9876543210, -100, false, -100),
    (0xfedcba9876543210, -100, true, -100),
    (0xfedcba9876543210, -150, false, -100),
    (0xfedcba9876543210, -150, true, -150),
    (0xfedcba9876543210, -12345, false, -12300),
    (0xfedcba9876543210, -12345, true, -12345),
    (0xfedcba9876543210, 9999999999, false, 10000000000),
    (0xfedcba9876543210, 9999999999, true, 10000000000),
    (0xfedcba9876543210, 10000000000, false, 10000000000),
    (0xfedcba9876543210, 10000000000, true, 10000000000),
    (0xfedcba9876543210, 10000000037, false, 10000000000),
    (0xfedcba9876543210, 10000000037, true, 10000000035),
    (0xfedcba9876543210, 100858800000000, false, 100858800000000),
    (0xfedcba9876543210, 100858800000000, true, 100858800000000),
    (0xfedcba9876543210, 100858800000030, false, 100858800000000),
    (0xfedcba9876543210, 100858800000030, true, 100858800000030),
    (0xfedcba9876543210, 100858800000077, false, 100858800000100),
    (0xfedcba9876543210, 100858800000077, true, 100858800000075),
    (0xfedcba9876543210, 100858800012345, false, 100858800012300),
    (0xfedcba9876543210, 100858800012345, true, 100858800012345),
    (0xfedcba9876543210, 1789718996556837, false, 1789718996556800),
    (0xfedcba9876543210, 1789718996556837, true, 1789718996556840),
    (0xfedcba9876543210, 1789718996556899, false, 1789718996556900),
    (0xfedcba9876543210, 1789718996556899, true, 1789718996556900),
    (0xfedcba9876543210, 2600000000000051, false, 2600000000000000),
    (0xfedcba9876543210, 2600000000000051, true, 2600000000000050),
    (0x5a5a5a5a5a5a5a5a, 0, false, 0),
    (0x5a5a5a5a5a5a5a5a, 0, true, 0),
    (0x5a5a5a5a5a5a5a5a, 1, false, 0),
    (0x5a5a5a5a5a5a5a5a, 1, true, 0),
    (0x5a5a5a5a5a5a5a5a, 4, false, 0),
    (0x5a5a5a5a5a5a5a5a, 4, true, 5),
    (0x5a5a5a5a5a5a5a5a, 5, false, 0),
    (0x5a5a5a5a5a5a5a5a, 5, true, 5),
    (0x5a5a5a5a5a5a5a5a, 30, false, 0),
    (0x5a5a5a5a5a5a5a5a, 30, true, 30),
    (0x5a5a5a5a5a5a5a5a, 49, false, 100),
    (0x5a5a5a5a5a5a5a5a, 49, true, 50),
    (0x5a5a5a5a5a5a5a5a, 50, false, 100),
    (0x5a5a5a5a5a5a5a5a, 50, true, 50),
    (0x5a5a5a5a5a5a5a5a, 51, false, 100),
    (0x5a5a5a5a5a5a5a5a, 51, true, 50),
    (0x5a5a5a5a5a5a5a5a, 99, false, 100),
    (0x5a5a5a5a5a5a5a5a, 99, true, 100),
    (0x5a5a5a5a5a5a5a5a, 100, false, 100),
    (0x5a5a5a5a5a5a5a5a, 100, true, 100),
    (0x5a5a5a5a5a5a5a5a, 101, false, 100),
    (0x5a5a5a5a5a5a5a5a, 101, true, 100),
    (0x5a5a5a5a5a5a5a5a, 150, false, 100),
    (0x5a5a5a5a5a5a5a5a, 150, true, 150),
    (0x5a5a5a5a5a5a5a5a, 199, false, 200),
    (0x5a5a5a5a5a5a5a5a, 199, true, 200),
    (0x5a5a5a5a5a5a5a5a, 200, false, 200),
    (0x5a5a5a5a5a5a5a5a, 200, true, 200),
    (0x5a5a5a5a5a5a5a5a, 250, false, 300),
    (0x5a5a5a5a5a5a5a5a, 250, true, 250),
    (0x5a5a5a5a5a5a5a5a, 999, false, 1000),
    (0x5a5a5a5a5a5a5a5a, 999, true, 1000),
    (0x5a5a5a5a5a5a5a5a, 1000, false, 1000),
    (0x5a5a5a5a5a5a5a5a, 1000, true, 1000),
    (0x5a5a5a5a5a5a5a5a, 12345, false, 12300),
    (0x5a5a5a5a5a5a5a5a, 12345, true, 12345),
    (0x5a5a5a5a5a5a5a5a, 99999, false, 100000),
    (0x5a5a5a5a5a5a5a5a, 99999, true, 99995),
    (0x5a5a5a5a5a5a5a5a, -1, false, 0),
    (0x5a5a5a5a5a5a5a5a, -1, true, 0),
    (0x5a5a5a5a5a5a5a5a, -30, false, 0),
    (0x5a5a5a5a5a5a5a5a, -30, true, -30),
    (0x5a5a5a5a5a5a5a5a, -99, false, -100),
    (0x5a5a5a5a5a5a5a5a, -99, true, -100),
    (0x5a5a5a5a5a5a5a5a, -100, false, -100),
    (0x5a5a5a5a5a5a5a5a, -100, true, -100),
    (0x5a5a5a5a5a5a5a5a, -150, false, -100),
    (0x5a5a5a5a5a5a5a5a, -150, true, -150),
    (0x5a5a5a5a5a5a5a5a, -12345, false, -12300),
    (0x5a5a5a5a5a5a5a5a, -12345, true, -12345),
    (0x5a5a5a5a5a5a5a5a, 9999999999, false, 10000000000),
    (0x5a5a5a5a5a5a5a5a, 9999999999, true, 10000000000),
    (0x5a5a5a5a5a5a5a5a, 10000000000, false, 10000000000),
    (0x5a5a5a5a5a5a5a5a, 10000000000, true, 10000000000),
    (0x5a5a5a5a5a5a5a5a, 10000000037, false, 10000000000),
    (0x5a5a5a5a5a5a5a5a, 10000000037, true, 10000000035),
    (0x5a5a5a5a5a5a5a5a, 100858800000000, false, 100858800000000),
    (0x5a5a5a5a5a5a5a5a, 100858800000000, true, 100858800000000),
    (0x5a5a5a5a5a5a5a5a, 100858800000030, false, 100858800000000),
    (0x5a5a5a5a5a5a5a5a, 100858800000030, true, 100858800000030),
    (0x5a5a5a5a5a5a5a5a, 100858800000077, false, 100858800000100),
    (0x5a5a5a5a5a5a5a5a, 100858800000077, true, 100858800000075),
    (0x5a5a5a5a5a5a5a5a, 100858800012345, false, 100858800012400),
    (0x5a5a5a5a5a5a5a5a, 100858800012345, true, 100858800012345),
    (0x5a5a5a5a5a5a5a5a, 1789718996556837, false, 1789718996556900),
    (0x5a5a5a5a5a5a5a5a, 1789718996556837, true, 1789718996556835),
    (0x5a5a5a5a5a5a5a5a, 1789718996556899, false, 1789718996556900),
    (0x5a5a5a5a5a5a5a5a, 1789718996556899, true, 1789718996556900),
    (0x5a5a5a5a5a5a5a5a, 2600000000000051, false, 2600000000000100),
    (0x5a5a5a5a5a5a5a5a, 2600000000000051, true, 2600000000000050),
        ];
        for &(secret, t, isolated, want) in VECTORS {
            let res = if isolated { FINE_RESOLUTION_US } else { COARSE_RESOLUTION_US };
            assert_eq!(clamp_us(t, res, secret), want, "secret {secret:#018x}, {t} µs, isolated {isolated}");
        }
    }

    /// One secret per PROCESS, as Blink's `static TimeClamper` is one per renderer: every clock -- the page's, a
    /// frame's, a worker's on its own thread -- clamps the same absolute tick the same way.
    #[test]
    fn the_clampers_secret_is_one_per_process() {
        let here = (clamper_secret(), PerfClock::default().secret);
        let there = std::thread::spawn(|| (clamper_secret(), PerfClock::default().secret, PerfClock::since_process_ms(0.0).secret)).join().unwrap();
        assert_eq!(here.0, here.1);
        assert_eq!((here.0, here.0, here.0), there);
        // and a page timestamp and a worker's for the same absolute time are the same number
        let a = clamp_us(100_858_800_000_042, COARSE_RESOLUTION_US, here.0);
        let b = std::thread::spawn(|| clamp_us(100_858_800_000_042, COARSE_RESOLUTION_US, clamper_secret())).join().unwrap();
        assert_eq!(a, b);
    }

    /// Unbiased, as Blink's is: a time a fraction `f` into its tick reads the next tick in a share `f` of the
    /// ticks, and a reading is never more than one tick from the time.
    #[test]
    fn a_time_rounds_up_in_proportion_to_its_fraction() {
        const N: i64 = 20_000;
        for f in [10i64, 30, 50, 90] {
            let ups = (0..N)
                .filter(|k| {
                    let t = 100_858_800_000_000 + k * COARSE_RESOLUTION_US + f;
                    let c = clamp_us(t, COARSE_RESOLUTION_US, TEST_SECRET);
                    assert!(c == t - f || c == t - f + COARSE_RESOLUTION_US, "{t} -> {c}");
                    c != t - f
                })
                .count() as f64;
            let share = ups / N as f64;
            assert!((share - f as f64 / 100.0).abs() < 0.02, "{f} µs into the tick: up {share}");
        }
    }

    /// The defect FP4 names, stated as a property: a real browser's readings are NOT exact decimal
    /// multiples of 0.1, and korvus's were. Over 250 consecutive ticks, exactly the ticks that land on
    /// a multiple of 0.5 ms come out exact — one in five — and the other four in five carry the
    /// origin's own rounding. (Chrome 150 / Brave 151, 600 readings each, 2026-09-18: same ratio.)
    #[test]
    fn four_readings_in_five_are_not_an_exact_decimal_tenth() {
        let c = clock_at(100_858_800_000_000);
        let g = ulp(c.start_abs_us as f64 / 1000.0);
        let mut exact = 0usize;
        let mut inexact = 0usize;
        for k in 0i64..250 {
            let v = c.dom_hrt(k * COARSE_RESOLUTION_US);
            // "An exact decimal tenth" is exactly what `String(v)` shows: v * 10 is a whole number
            // AND v is the double nearest it, i.e. the tick is representable. That is k % 5 == 0.
            let on_a_half = k % 5 == 0;
            let is_exact = v * 10.0 == (v * 10.0).round() && (v * 10.0).round() as i64 == k;
            assert_eq!(is_exact, on_a_half, "tick {k} -> {v:?}");
            if is_exact {
                exact += 1
            } else {
                inexact += 1
            }
            // Whatever the residue, it is under one grid step — a shape, never a wrong time.
            assert!((v - k as f64 / 10.0).abs() < g, "tick {k} -> {v:?} is more than one ULP off");
        }
        assert_eq!((exact, inexact), (50, 200));
    }

    /// Every reading lands on the grid of the monotonic clock's own millisecond value, and the
    /// residues around the 0.1 ms ladder take exactly five values. That five is the signature: it is
    /// what `0.4k mod 1` plus one constant origin offset produces, and it is what both browsers show.
    #[test]
    fn the_residues_are_the_five_the_browsers_show() {
        // Three magnitudes, i.e. three plausible uptimes — minutes, a day, a month. The grid moves
        // with the magnitude (that is the point) and the FIVE does not.
        for abs_us in [400_000_000i64, 100_858_800_000_000, 2_600_000_000_000_000] {
            let c = clock_at(abs_us);
            let g = ulp(abs_us as f64 / 1000.0);
            let mut residues: Vec<i64> = Vec::new();
            for k in 0i64..250 {
                let v = c.dom_hrt(k * COARSE_RESOLUTION_US);
                // On the grid, exactly: the difference of two doubles of the same binade is exact.
                let units = v / g;
                assert_eq!(units, units.round(), "{abs_us}: tick {k} -> {v:?} is off the clock's grid");
                let r = ((v - k as f64 / 10.0) / (g / 5.0)).round() as i64;
                if !residues.contains(&r) {
                    residues.push(r);
                }
            }
            residues.sort_unstable();
            assert_eq!(residues.len(), 5, "{abs_us}: residues {residues:?}");
            // They are the five fifths of a grid step, shifted by the origin's own rounding: the gaps
            // between consecutive residues are all one fifth.
            for w in residues.windows(2) {
                assert_eq!(w[1] - w[0], 1, "{abs_us}: residues {residues:?} are not five consecutive fifths");
            }
            assert!(residues.contains(&0), "{abs_us}: the exact tick is missing from {residues:?}");
        }
    }

    /// Still a clock: monotonic, starting at the realm's origin, and signed on the way back.
    #[test]
    fn monotonic_from_zero_and_signed_before_the_origin() {
        let c = clock_at(100_858_800_000_000);
        assert_eq!(c.dom_hrt(0), 0.0);
        let mut prev = f64::NEG_INFINITY;
        for raw in (0i64..2_000_000).step_by(37) {
            let v = c.dom_hrt(raw);
            assert!(v >= prev, "{raw} went backwards: {v} < {prev}");
            prev = v;
        }
        // A resource the transport measured before this realm existed reads negative: the tick below or the
        // next, as any other time (the clamp works on the absolute monotonic time).
        let v = c.dom_hrt(-150);
        let g = ulp(c.start_abs_us as f64 / 1000.0);
        assert!((v + 0.2).abs() <= g || (v + 0.1).abs() <= g, "-150 µs -> {v:?}");
    }

    /// `timeOrigin` is on the same 100 µs quantum as `now()`. It was not: korvus printed
    /// `1789718898281.8726` where Chrome prints `1789718996556.8`, four digits of real sub-quantum
    /// precision that no browser exposes.
    #[test]
    fn the_time_origin_is_quantized_to_the_same_hundred_microseconds() {
        let o = PerfClock::default().origin_ms();
        assert!(o > 1_500_000_000_000.0, "timeOrigin {o} is not an epoch millisecond count");
        // ×10 is a whole number, and there is nothing under it: at this magnitude a double resolves
        // ~244 ns, so any finer reading would have SHOWN.
        assert_eq!(o * 10.0, (o * 10.0).round(), "timeOrigin {o:?} is finer than 100 µs");
    }

    /// The realm's own origin is where the clock starts, whatever the process-wide zero is: two clocks
    /// born a moment apart both read ~0 at birth, and each counts from itself.
    #[test]
    fn each_realm_counts_from_its_own_origin() {
        let a = PerfClock::default();
        let mut spin = 0u64;
        while a.now_ms() < 1.0 {
            spin = spin.wrapping_add(1);
        }
        assert!(spin > 0);
        let b = PerfClock::default();
        assert!(b.now_ms() < a.now_ms(), "the younger realm's clock is not behind the older one's");
        assert!(b.now_ms() >= 0.0 && a.now_ms() >= 1.0);
    }
}
