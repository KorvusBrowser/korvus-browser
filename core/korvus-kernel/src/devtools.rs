//! What a DevTools client set on this page process: the overrides CDP's `Emulation.*`, `Network.*`, `Page.*` and
//! `Browser.*` commands ask for, as Chrome keeps them per page for as long as its client says so.
//!
//! WHY THEY LIVE HERE. Each override is read by more than one layer -- the user agent by `navigator` (webapi) and by
//! the request headers (transport), the viewport by layout, `@media` and the painted frame -- and every one of those
//! crates already reads the persona through this one. A page target is a process of its own (`cdp::router`), so a
//! process-wide value is exactly the page's: no override reaches another page, another context or another client.
//!
//! WHEN NONE IS SET, NOTHING CHANGES. Every reader passes the persona's value through [`navigator`],
//! [`client_hints`], [`screen`] and friends, which return it untouched until a client sets something -- the page a
//! run with no DevTools client sees is byte-for-byte the one it saw before this file existed (invariant I9/J2).
//!
//! WHAT AN OVERRIDE DOES TO THE IDENTITY. The persona owns the identity (D11, D12), and a client that overrides the
//! user agent replaces part of it -- on purpose, as Chrome's `setUserAgentOverride` does: the strings change and the
//! engine under them does not, so a script that probes the engine sees the build the engine is. The override is the
//! client's statement, made explicitly; korvus never makes one on its own (decisions.md D100).
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::RwLock;

use crate::{Brand, ClientHints, NavigatorProfile, ScreenProfile};

/// `Emulation.setUserAgentOverride` / `Network.setUserAgentOverride`.
#[derive(Clone, Debug, Default)]
pub struct UaOverride {
    /// The `User-Agent` and `navigator.userAgent`. Empty: the persona's (a client that only set a language).
    pub user_agent: String,
    /// `acceptLanguage`: the `Accept-Language` list (`de-DE,de`), and `navigator.languages`. Empty: the persona's.
    pub accept_language: String,
    /// `platform`: `navigator.platform`. Empty: the persona's.
    pub platform: String,
    /// `userAgentMetadata`: the client hints. `None` leaves the persona's, as Chrome leaves its own (see
    /// [`client_hints`]).
    pub metadata: Option<UaMetadata>,
}

/// `Emulation.UserAgentMetadata`.
#[derive(Clone, Debug, Default)]
pub struct UaMetadata {
    pub brands: Vec<Brand>,
    pub full_version_list: Vec<Brand>,
    pub full_version: String,
    pub platform: String,
    pub platform_version: String,
    pub architecture: String,
    pub model: String,
    pub mobile: bool,
    pub bitness: String,
    pub wow64: bool,
    pub form_factors: Vec<String>,
}

/// `Emulation.setDeviceMetricsOverride`: the viewport (CSS px), the device pixel ratio and the screen. A zero is
/// "not overridden" for that value, as the protocol's own zero is.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DeviceMetrics {
    pub width: f64,
    pub height: f64,
    pub device_scale_factor: f64,
    pub mobile: bool,
    pub screen_width: f64,
    pub screen_height: f64,
}

/// `Emulation.setEmulatedMedia`: the media type (`print`, `screen`, `""` for none) and the features asked about.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EmulatedMedia {
    pub media: String,
    pub features: Vec<(String, String)>,
}

/// `Emulation.setGeolocationOverride`: a position, or `None` inside for "position unavailable".
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geolocation {
    pub latitude: f64,
    pub longitude: f64,
    pub accuracy: f64,
}

static UA: RwLock<Option<UaOverride>> = RwLock::new(None);
static METRICS: RwLock<Option<DeviceMetrics>> = RwLock::new(None);
static MEDIA: RwLock<Option<EmulatedMedia>> = RwLock::new(None);
static GEO: RwLock<Option<Option<Geolocation>>> = RwLock::new(None);
static PERMISSIONS: RwLock<Option<Vec<String>>> = RwLock::new(None);
/// `Emulation.setTouchEmulationEnabled`: the touch points emulated, `-1` for none.
static TOUCH: AtomicI32 = AtomicI32::new(-1);
static SCRIPTS_OFF: AtomicBool = AtomicBool::new(false);
static BYPASS_CSP: AtomicBool = AtomicBool::new(false);
static OFFLINE: AtomicBool = AtomicBool::new(false);
static IGNORE_CERT_ERRORS: AtomicBool = AtomicBool::new(false);
static EXTRA_HEADERS: RwLock<Vec<(String, String)>> = RwLock::new(Vec::new());
static BACKGROUND: RwLock<Option<[u8; 4]>> = RwLock::new(None);
static DOWNLOADS: RwLock<Option<(String, String)>> = RwLock::new(None);

fn read<T: Clone>(l: &RwLock<T>) -> T {
    l.read().unwrap_or_else(|e| e.into_inner()).clone()
}

fn write<T>(l: &RwLock<T>, v: T) {
    *l.write().unwrap_or_else(|e| e.into_inner()) = v;
}

pub fn set_user_agent(o: Option<UaOverride>) {
    write(&UA, o.filter(|o| !(o.user_agent.is_empty() && o.accept_language.is_empty() && o.platform.is_empty())));
}

pub fn user_agent() -> Option<UaOverride> {
    read(&UA)
}

pub fn set_device_metrics(m: Option<DeviceMetrics>) {
    write(&METRICS, m);
}

pub fn device_metrics() -> Option<DeviceMetrics> {
    read(&METRICS)
}

pub fn set_emulated_media(m: Option<EmulatedMedia>) {
    write(&MEDIA, m.filter(|m| !m.media.is_empty() || m.features.iter().any(|(_, v)| !v.is_empty())));
}

pub fn emulated_media() -> Option<EmulatedMedia> {
    read(&MEDIA)
}

/// The value an emulated media feature was given (`prefers-color-scheme` -> `dark`), when one was.
pub fn media_feature(name: &str) -> Option<String> {
    read(&MEDIA).and_then(|m| m.features.into_iter().find(|(n, v)| n == name && !v.is_empty()).map(|(_, v)| v))
}

/// `Some(None)` is an override with no position (`setGeolocationOverride {}`): the page is told none is available.
pub fn set_geolocation(g: Option<Option<Geolocation>>) {
    write(&GEO, g);
}

pub fn geolocation() -> Option<Option<Geolocation>> {
    read(&GEO)
}

/// `Browser.grantPermissions`' names for this page's origin; `None` is `Browser.resetPermissions`.
pub fn set_permissions(p: Option<Vec<String>>) {
    write(&PERMISSIONS, p);
}

/// `granted` for a name a client granted (`Browser.grantPermissions`); `None` leaves the page's own answer.
pub fn permission(name: &str) -> Option<&'static str> {
    read(&PERMISSIONS)?.iter().any(|g| g == name).then_some("granted")
}

pub fn set_touch_points(n: Option<i32>) {
    TOUCH.store(n.unwrap_or(-1), Ordering::Relaxed);
}

pub fn touch_points() -> Option<i32> {
    let n = TOUCH.load(Ordering::Relaxed);
    (n >= 0).then_some(n)
}

pub fn set_scripts_disabled(off: bool) {
    SCRIPTS_OFF.store(off, Ordering::Relaxed);
}

/// `Emulation.setScriptExecutionDisabled`: the document's own scripts do not run (a client's evaluations still do).
pub fn scripts_disabled() -> bool {
    SCRIPTS_OFF.load(Ordering::Relaxed)
}

pub fn set_bypass_csp(on: bool) {
    BYPASS_CSP.store(on, Ordering::Relaxed);
}

/// `Page.setBypassCSP`: no Content Security Policy is enforced on the page.
pub fn bypass_csp() -> bool {
    BYPASS_CSP.load(Ordering::Relaxed)
}

pub fn set_offline(on: bool) {
    OFFLINE.store(on, Ordering::Relaxed);
}

/// `Network.emulateNetworkConditions {offline: true}`: no request reaches the network, and `navigator.onLine` is false.
pub fn offline() -> bool {
    OFFLINE.load(Ordering::Relaxed)
}

pub fn set_ignore_cert_errors(on: bool) {
    IGNORE_CERT_ERRORS.store(on, Ordering::Relaxed);
}

/// `Security.setIgnoreCertificateErrors`: a certificate the trust store rejects is used anyway.
pub fn ignore_cert_errors() -> bool {
    IGNORE_CERT_ERRORS.load(Ordering::Relaxed)
}

/// `Network.setExtraHTTPHeaders`: headers every request of the page carries, over its own of the same name.
pub fn set_extra_headers(h: Vec<(String, String)>) {
    write(&EXTRA_HEADERS, h);
}

pub fn extra_headers() -> Vec<(String, String)> {
    read(&EXTRA_HEADERS)
}

/// `Emulation.setDefaultBackgroundColorOverride`: the colour under the page in its pictures (`[r, g, b, a]`; an alpha
/// of 0 leaves what the page does not paint transparent). `None` is Chrome's white.
pub fn set_background(c: Option<[u8; 4]>) {
    write(&BACKGROUND, c);
}

pub fn background() -> Option<[u8; 4]> {
    read(&BACKGROUND)
}

/// `Browser.setDownloadBehavior`: `(behavior, downloadPath)` -- `allow` saves a download under its suggested name,
/// `allowAndName` under its guid (Playwright's), `deny` refuses it. `None`: nobody asked, and nothing is downloaded.
pub fn set_download_behavior(b: Option<(String, String)>) {
    write(&DOWNLOADS, b.filter(|(b, _)| b != "default"));
}

pub fn download_behavior() -> Option<(String, String)> {
    read(&DOWNLOADS)
}

/// `navigator.languages` from an `acceptLanguage` list: each tag, its `;q=` weight dropped.
pub fn languages_of(accept_language: &str) -> Vec<String> {
    accept_language.split(',').map(|t| t.split(';').next().unwrap_or("").trim().to_string()).filter(|t| !t.is_empty()).collect()
}

/// The `Accept-Language` header for a language list, weighted as Chrome's `GenerateAcceptLanguageHeader` writes it:
/// the first tag bare, each later one a tenth lower, never below `0.1`.
pub fn accept_language_header(langs: &[String]) -> String {
    let mut q = 10i32;
    let mut out = Vec::with_capacity(langs.len());
    for (i, l) in langs.iter().enumerate() {
        if i == 0 {
            out.push(l.clone());
        } else {
            q = (q - 1).max(1);
            out.push(format!("{l};q=0.{q}"));
        }
    }
    out.join(",")
}

/// The persona's navigator as the page sees it: the client's user agent, platform and languages over it.
pub fn navigator(mut n: NavigatorProfile) -> NavigatorProfile {
    if let Some(o) = user_agent() {
        if !o.user_agent.is_empty() {
            n.user_agent = o.user_agent;
        }
        if !o.platform.is_empty() {
            n.platform = o.platform;
        }
        if !o.accept_language.is_empty() {
            n.languages = languages_of(&o.accept_language);
            n.accept_language = accept_language_header(&n.languages);
        }
    }
    n
}

/// The persona's client hints as the page and the wire see them: the client's `userAgentMetadata` over them when it
/// gave one. A user agent overridden WITHOUT metadata leaves the hints as they were -- Chrome 152, measured: a
/// `setUserAgentOverride` naming Chrome 150 and no metadata still sends `sec-ch-ua` for 152 and answers
/// `navigator.userAgentData` with 152 -- so a client that wants both to agree passes the metadata.
pub fn client_hints(mut ch: ClientHints) -> ClientHints {
    let Some(m) = user_agent().filter(|o| !o.user_agent.is_empty()).and_then(|o| o.metadata) else { return ch };
    ch.brands = m.brands;
    ch.full_version_list = m.full_version_list;
    ch.ua_full_version = m.full_version;
    ch.platform = m.platform;
    ch.platform_version = m.platform_version;
    ch.architecture = m.architecture;
    ch.model = m.model;
    ch.mobile = m.mobile;
    ch.bitness = m.bitness;
    ch.wow64 = m.wow64;
    if !m.form_factors.is_empty() {
        ch.form_factors = m.form_factors;
    }
    ch
}

/// The persona's screen as the page sees it: the emulated screen size and device pixel ratio over it.
pub fn screen(mut s: ScreenProfile) -> ScreenProfile {
    if let Some(m) = device_metrics() {
        if m.device_scale_factor > 0.0 {
            s.device_pixel_ratio = m.device_scale_factor;
        }
        if m.screen_width > 0.0 && m.screen_height > 0.0 {
            s.width = m.screen_width.round() as u32;
            s.height = m.screen_height.round() as u32;
            s.avail_width = s.width;
            s.avail_height = s.height;
            s.avail_left = 0;
            s.avail_top = 0;
        }
    }
    s
}

/// The persona's structural navigator values as the page sees them: the emulated touch points over them.
pub fn navigator_extra(mut e: crate::NavigatorExtra) -> crate::NavigatorExtra {
    if let Some(n) = touch_points() {
        e.max_touch_points = n.max(0) as u32;
    }
    e
}

/// The emulated viewport, `(width, height)` in CSS px, when a client set one.
pub fn viewport() -> Option<(f64, f64)> {
    device_metrics().filter(|m| m.width > 0.0 && m.height > 0.0).map(|m| (m.width, m.height))
}

/// Forget every override: the page as its persona made it.
pub fn reset() {
    set_user_agent(None);
    set_device_metrics(None);
    set_emulated_media(None);
    set_geolocation(None);
    set_permissions(None);
    set_touch_points(None);
    set_scripts_disabled(false);
    set_bypass_csp(false);
    set_offline(false);
    set_ignore_cert_errors(false);
    set_extra_headers(Vec::new());
    set_background(None);
    set_download_behavior(None);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_accept_language_list_is_weighted_as_chrome_writes_it() {
        assert_eq!(accept_language_header(&languages_of("de-DE")), "de-DE");
        assert_eq!(accept_language_header(&languages_of("de-DE, en;q=0.5,fr")), "de-DE,en;q=0.9,fr;q=0.8");
    }
}
