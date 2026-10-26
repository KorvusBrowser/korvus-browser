//! The Coherence Engine. `audit(&dyn Persona)` returns the list of cross-layer
//! contradictions; an empty list means one coherent identity. A "Frankenstein" persona (Mac UA +
//! Windows platform, Chrome UA + a mismatched TLS major, a Mac claiming an Adreno GPU) is an instant
//! bot tell, so the binary audits the assembled persona at launch and a test asserts a mixed persona
//! FAILS.
//!
//! Signals threaded: UA os-family <-> `navigator.platform` <-> UA-CH platform <-> WebGL GPU vendor;
//! UA Chrome-major <-> UA-CH brand version <-> UA-CH `uaFullVersion` <-> transport TLS profile id.

use crate::Persona;

/// One cross-layer contradiction found by the audit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incoherence {
    /// Which two layers disagree (e.g. `"UA<->navigator.platform"`).
    pub layers: &'static str,
    pub detail: String,
}

/// The persona OS family a user agent names (`"windows"`, `"mac"`, `"linux"`, `"android"`, `"ios"`),
/// or `None` for a UA that names none. Public so the persona crate can read a loaded bundle's family
/// when it picks the build an OS wears.
pub fn os_family_from_ua(ua: &str) -> Option<&'static str> {
    if ua.contains("Android") {
        Some("android")
    } else if ua.contains("iPhone") || ua.contains("iPad") {
        Some("ios")
    } else if ua.contains("Macintosh") || ua.contains("Mac OS X") {
        Some("mac")
    } else if ua.contains("Windows") {
        Some("windows")
    } else if ua.contains("Linux") || ua.contains("X11") {
        Some("linux")
    } else {
        None
    }
}

fn chrome_major(ua: &str) -> Option<u32> {
    ua.split("Chrome/").nth(1)?.split('.').next()?.parse().ok()
}

/// Audit a persona for cross-layer coherence. An empty vec means the identity is internally consistent
/// across the transport, JS-surface, and device layers.
pub fn audit(p: &dyn Persona) -> Vec<Incoherence> {
    let mut out = Vec::new();
    let nav = p.navigator();
    let ua = nav.user_agent.clone();
    let hints = p.client_hints();
    let tls = p.transport_profile();

    let mut bad = |layers: &'static str, detail: String| out.push(Incoherence { layers, detail });

    let os = os_family_from_ua(&ua);

    // UA os-family <-> navigator.platform / UA-CH platform / WebGL GPU vendor.
    if let Some(os) = os {
        let plat = nav.platform.as_str();
        let plat_ok = match os {
            "mac" => plat == "MacIntel",
            "windows" => plat.starts_with("Win"),
            "linux" => plat.contains("Linux") || plat.contains("x86"),
            "android" => plat.starts_with("Linux") || plat.contains("arm") || plat.contains("aarch"),
            "ios" => plat == "iPhone" || plat == "iPad",
            _ => true,
        };
        if !plat_ok {
            bad("UA<->navigator.platform", format!("UA is {os} but navigator.platform={plat:?}"));
        }

        let want_chp = match os {
            "mac" => "macOS",
            "windows" => "Windows",
            "linux" => "Linux",
            "android" => "Android",
            "ios" => "iOS",
            _ => "",
        };
        if !hints.platform.is_empty() && !want_chp.is_empty() && hints.platform != want_chp {
            bad(
                "UA<->UA-CH.platform",
                format!("UA is {os} but userAgentData.platform={:?} (want {want_chp:?})", hints.platform),
            );
        }

        let oracle = p.device_oracle_json();
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&oracle) {
            if let Some(r) = v
                .get("webgl")
                .and_then(|w| w.get("unmaskedRenderer"))
                .and_then(|s| s.as_str())
            {
                let gpu_ok = match os {
                    "mac" | "ios" => r.contains("Apple") || r.contains("Metal"),
                    "windows" => r.contains("Direct3D") || r.contains("ANGLE") || r.contains("D3D"),
                    "android" => r.contains("Adreno") || r.contains("Mali") || r.contains("PowerVR"),
                    _ => true,
                };
                if !gpu_ok {
                    bad("UA<->WebGL.renderer", format!("UA is {os} but WebGL unmaskedRenderer={r:?}"));
                }
            }
        }
    }

    // UA Chrome-major <-> UA-CH brands / uaFullVersion <-> TLS profile id.
    if let Some(major) = chrome_major(&ua) {
        let brand_major = hints.brands.iter().find_map(|b| {
            if b.brand.contains("Chrome") || b.brand == "Chromium" {
                b.version.split('.').next()?.parse::<u32>().ok()
            } else {
                None
            }
        });
        if let Some(bm) = brand_major {
            if bm != major {
                bad("UA<->UA-CH.brands", format!("UA Chrome/{major} but brand version {bm}"));
            }
        }
        // `uaFullVersion` NAMES THE PRODUCT, AND THE PRODUCT IS NOT ALWAYS CHROMIUM.
        //
        // This compared `uaFullVersion`'s major to the UA's `Chrome/<n>` and flagged any difference.
        // That is right for every browser whose product version IS its Chromium version -- Chrome,
        // Brave and Edge all are -- and it is wrong for Opera, which is the whole reason a fourth
        // persona was captured. Opera 135 sends `Chrome/151.0.0.0 ... OPR/135.0.0.0` and answers
        // `getHighEntropyValues(["uaFullVersion"])` with `135.0.5973.133`: its own version, measured
        // off the real browser, not a defect and not this engine's invention. Under the
        // old rule its bundle could not boot at all.
        //
        // The rule that holds for all four without naming a browser: `uaFullVersion` must be one of
        // the versions the persona's own `fullVersionList` declares. Chrome answers 150.0.7871.46 and
        // lists it; Opera answers 135.0.5973.133 and lists it beside Chromium's 151.0.7922.176. A
        // value in neither list is still what it always was -- a version this device does not claim
        // anywhere else -- and is still flagged, which is the case this check was written for.
        //
        // Asking "which brand is this browser" instead would put a browser-name branch in engine
        // code, which is exactly what `scripts/persona_literals.py` exists to keep out.
        if !hints.ua_full_version.is_empty() {
            let declared: Vec<&str> = hints.full_version_list.iter().map(|b| b.version.as_str()).collect();
            let listed = declared.iter().any(|v| *v == hints.ua_full_version);
            if !listed && !declared.is_empty() {
                bad(
                    "UA<->UA-CH.uaFullVersion",
                    format!(
                        "uaFullVersion {} is not one of the versions fullVersionList declares ({})",
                        hints.ua_full_version,
                        declared.join(", ")
                    ),
                );
            } else if declared.is_empty() {
                // No list to check against, so fall back to the UA's own major -- the pre-L7f rule,
                // kept for a persona that records `uaFullVersion` and nothing else.
                if let Some(fm) = hints.ua_full_version.split('.').next().and_then(|s| s.parse::<u32>().ok()) {
                    if fm != major {
                        bad(
                            "UA<->UA-CH.uaFullVersion",
                            format!("UA Chrome/{major} but uaFullVersion {} and no fullVersionList to check it against",
                                    hints.ua_full_version),
                        );
                    }
                }
            }
        }
        // UA Chrome-major <-> TLS. The preset `id` (`chrome_149`) NAMES a wreq-util emulation preset, not the version
        // on the wire: the pinned library ships no `chrome` preset above 149, so a Chrome-152 persona rides the 149
        // base and reconciles the gap with bytes it MEASURED off the real browser -- the `trust_anchors` extension a
        // Chrome-152 ClientHello carries (0xca34), the HTTP/2 priority, the QUIC preset. Comparing the base preset's
        // trailing integer to the UA major then cries wolf over a ClientHello that is JA4-identical to the real one
        // (the bundle manifests' `tls-version` entry documents this and asks for exactly this fix: the audit has no
        // socket, so it compares the wire it can SEE -- the measured fields -- not the integer in the preset's name).
        // With none of them measured, the preset name is all there is, and its integer is still a signal (the
        // negative-control personas in `quality.rs` ride a bare old preset and are still flagged).
        if let Some(tls_major) = tls.id.rsplit('_').next().and_then(|s| s.parse::<u32>().ok()) {
            let wire_measured = tls.trust_anchors.is_some() || tls.h2_priority.is_some() || tls.quic.is_some();
            if tls_major != major && !wire_measured {
                bad(
                    "UA<->TLS.profile",
                    format!("UA Chrome/{major} but transport profile {:?} and no measured wire (trust_anchors/h2/quic) to reconcile it", tls.id),
                );
            }
        }
    }

    if nav.languages.is_empty() {
        bad("navigator.languages", "empty languages list".to_string());
    }

    out
}

/// The contradictions this persona has NOT accounted for — what a hard gate should act on.
///
/// [`audit`] reports every disagreement it can see, which is the right answer for a report and the
/// wrong one for a gate: a tree mid-migration has known, written-down gaps, and stopping on those
/// would stop everything. `Persona::known_incoherences` is the persona's own list of what it has
/// accounted for; anything outside it is new, unexplained, and worth refusing to boot over.
///
/// The mapping from a finding to a declared id is by LAYERS, never by message text. The message is
/// written for a human reading a boot failure and will be reworded; the pair of layers that disagree
/// is the stable identity of the disagreement.
pub fn unaccounted(p: &dyn Persona) -> Vec<Incoherence> {
    unaccounted_on(p, &HostFacts::measure())
}

/// [`unaccounted`], including the checks against a given host.
pub fn unaccounted_on(p: &dyn Persona, host: &HostFacts) -> Vec<Incoherence> {
    let declared = p.known_incoherences();
    audit(p)
        .into_iter()
        .chain(audit_host(p, host))
        .filter(|i| !declared.iter().any(|d| d == incoherence_id(i)))
        .collect()
}

/// The stable id of a finding, derived from which layers disagree.
pub fn incoherence_id(i: &Incoherence) -> &'static str {
    let l = i.layers;
    if l.contains("TLS") || l.contains("transport") {
        "tls-version"
    } else if l.contains("WebGL") || l.contains("GPU") {
        "webgl-pixels"
    } else if l.contains("platform") {
        "platform"
    } else if l.contains("host.os") {
        "host-os"
    } else if l.contains("host.cores") {
        "host-cores"
    } else if l.contains("brand") || l.contains("version") {
        "ua-version"
    } else {
        "unclassified"
    }
}

/// What the MACHINE says about itself — the other side of a comparison the audit never made.
///
/// Every check above compares the persona against itself: UA against platform, UA against UA-CH,
/// UA against the TLS preset. All of them pass on a bundle that describes a computer this process
/// is not running on, because none of them ever looks at the computer.
///
/// That gap has a cost that grows with every machine the project touches. The shipped bundle claims
/// twelve cores and `ANGLE (AMD, AMD Ryzen 5 9600X ...)`; those are statements about ONE box, and on
/// any other one they are false while the audit stays green. A page reads `hardwareConcurrency` and
/// times a workload; it reads the renderer string and hashes a frame. Neither needs our permission
/// to notice.
#[derive(Debug, Clone, Default)]
pub struct HostFacts {
    /// `std::env::consts::OS` — `"linux"`, `"macos"`, `"windows"`.
    pub os: String,
    /// The logical processors this process may run on, counted as Chrome counts them for
    /// `navigator.hardwareConcurrency` ([`chrome_processor_count`]), when it can be read.
    pub cores: Option<u32>,
    /// The machine's physical memory in MiB, as Chrome reads it for `navigator.deviceMemory`, when it can be read.
    pub memory_mib: Option<u64>,
}

impl HostFacts {
    /// Read what this process can cheaply know about the machine under it.
    pub fn measure() -> Self {
        HostFacts { os: std::env::consts::OS.to_string(), cores: chrome_processor_count(), memory_mib: physical_memory_mib() }
    }
}

/// `navigator.hardwareConcurrency` as Chrome computes it on this machine (`base::SysInfo::NumberOfProcessors`): on
/// Linux the configured logical processors restricted to this process's affinity mask -- the mask alone, not a
/// cgroup's CPU quota, which `std::thread::available_parallelism` also applies; elsewhere the logical processors.
pub fn chrome_processor_count() -> Option<u32> {
    #[cfg(target_os = "linux")]
    if let Some(n) = std::fs::read_to_string("/proc/self/status").ok().and_then(|s| {
        let list = s.lines().find_map(|l| l.strip_prefix("Cpus_allowed_list:"))?.trim().to_string();
        cpu_list_len(&list)
    }) {
        return Some(n);
    }
    std::thread::available_parallelism().ok().map(|n| n.get() as u32)
}

/// The number of processors in a kernel CPU list (`0-3,8,10-11`).
fn cpu_list_len(list: &str) -> Option<u32> {
    let mut n = 0u32;
    for part in list.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        n += match part.split_once('-') {
            Some((a, b)) => b.parse::<u32>().ok()?.checked_sub(a.parse::<u32>().ok()?)? + 1,
            None => part.parse::<u32>().map(|_| 1).ok()?,
        };
    }
    (n > 0).then_some(n)
}

/// The machine's physical memory in MiB as Chrome reads it (`sysconf(_SC_PHYS_PAGES) * _SC_PAGESIZE` on Linux, which
/// is `/proc/meminfo`'s `MemTotal`); `None` where this process cannot read it.
fn physical_memory_mib() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let s = std::fs::read_to_string("/proc/meminfo").ok()?;
        let kb: u64 = s.lines().find_map(|l| l.strip_prefix("MemTotal:"))?.trim().trim_end_matches("kB").trim().parse().ok()?;
        Some(kb / 1024)
    }
    #[cfg(not(target_os = "linux"))]
    None
}

/// `navigator.deviceMemory` for a machine with `mib` MiB, as Chrome 152 computes it
/// (`blink::ApproximatedDeviceMemory`): the power of two nearest to the memory (the lower one on a tie), in GiB,
/// clamped to `[2, 32]` -- the desktop limits of `kUpdatedDeviceMemoryLimitsFor2026`, on in Chrome 152 (measured: a
/// 32 GB Windows PC and a 60 GB Linux one both read 32).
pub fn chrome_device_memory(mib: u64) -> f64 {
    if mib == 0 {
        return 2.0;
    }
    let lower = 1u64 << (63 - mib.leading_zeros());
    let upper = lower << 1;
    let nearest = if mib - lower <= upper - mib { lower } else { upper };
    (nearest as f64 / 1024.0).clamp(2.0, 32.0)
}

/// The persona OS families a host OS may wear (owner 2026-10-17, a reference host-OS policy's map): a Windows persona on Windows
/// and on Linux -- Linux desktop Chrome is rare, and a Linux server should look like the commonest desktop -- a Mac
/// persona on macOS, and an Android persona on Linux (the same kernel). Every other pairing is a host mismatch.
pub fn host_may_wear(host_os: &str, persona_os: &str) -> bool {
    match host_os {
        "linux" | "android" => matches!(persona_os, "windows" | "linux" | "android"),
        "windows" => persona_os == "windows",
        "macos" => persona_os == "mac",
        other => persona_os == other,
    }
}

/// The persona audited against the machine it is actually running on.
///
/// Deliberately NARROW. Only facts a page can read and this process can measure without a rendering
/// context appear here: the OS family, and the core count. The GPU string and the font list are the
/// two that matter most and neither is available to the kernel -- the first needs a live GL context
/// and the second the render plugin's font manager -- so they stay declared in the bundle
/// (`webgl-pixels`, `fonts-host`) rather than pretended at here.
///
/// A CORE-COUNT MISMATCH IS NOT COSMETIC. `navigator.hardwareConcurrency` is read by every
/// fingerprinting script there is, and it is one of the few claims a page can CHECK: spawn that many
/// workers and time them. A persona claiming twelve cores on a four-core box is caught by arithmetic.
pub fn audit_host(p: &dyn Persona, host: &HostFacts) -> Vec<Incoherence> {
    let mut out = Vec::new();
    let nav = p.navigator();

    if let Some(os) = os_family_from_ua(&nav.user_agent) {
        if !host_may_wear(&host.os, os) {
            out.push(Incoherence {
                layers: "persona<->host.os",
                detail: format!(
                    "the persona is a {os} device and this process is running on {}. Every value it \
                     serves -- fonts, GPU, screen, core count -- describes a machine that is not here.",
                    host.os
                ),
            });
        }
    }

    // MORE cores than the machine has is the claim arithmetic catches; fewer -- a persona document's smaller device
    // (goal 1020 step 3, the shared desktop pool) -- is what a page cannot tell from a busy machine.
    if let (Some(hc), n) = (host.cores, nav.hardware_concurrency) {
        if n != 0 && n > hc {
            out.push(Incoherence {
                layers: "persona<->host.cores",
                detail: format!(
                    "the persona reports navigator.hardwareConcurrency={n} and this machine has {hc}. \
                     A page can check that by spawning workers and timing them."
                ),
            });
        }
    }
    out
}

#[cfg(test)]
mod host_tests {
    use super::*;

    #[test]
    fn device_memory_is_blinks_nearest_power_of_two_in_the_2026_limits() {
        assert_eq!(chrome_device_memory(61_968), 32.0, "this PC's MemTotal: 64 GiB is nearer, clamped to 32");
        assert_eq!(chrome_device_memory(32_536), 32.0, "a 32 GB machine");
        assert_eq!(chrome_device_memory(15_800), 16.0);
        assert_eq!(chrome_device_memory(12_288), 8.0, "a tie takes the lower power");
        assert_eq!(chrome_device_memory(1_024), 2.0, "the lower limit");
    }

    #[test]
    fn a_cpu_list_counts_its_processors() {
        assert_eq!(cpu_list_len("0-11"), Some(12));
        assert_eq!(cpu_list_len("0-3,8,10-11"), Some(7));
        assert_eq!(cpu_list_len(""), None);
        assert_eq!(cpu_list_len("x"), None);
    }

    #[test]
    fn the_host_os_map_matches_the_reference() {
        assert!(host_may_wear("linux", "windows"), "a Linux host wears a Windows persona (owner 2026-10-17)");
        assert!(host_may_wear("windows", "windows"));
        assert!(host_may_wear("macos", "mac"));
        assert!(host_may_wear("linux", "android"));
        assert!(!host_may_wear("windows", "linux"));
        assert!(!host_may_wear("linux", "mac"), "a Mac persona needs a Mac");
        assert!(!host_may_wear("macos", "windows"));
    }
}
