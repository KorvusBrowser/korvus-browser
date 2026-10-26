//! `core/navigator/network_information.rs` -- `NetworkInformation` and the `navigator.connection`
//! it answers (row C11 of the L14 findings).
//!
//! WHAT WAS HERE BEFORE. `NetworkInformation` was a generated-registry shell whose four attributes
//! were D2 stubs, and `Navigator.connection` was a `stub` row -- so under every bundle that records
//! the member, `typeof navigator.connection` was `"undefined"` and a page reading
//! `navigator.connection.effectiveType` threw (two corpus sites did, per the L14 findings).
//!
//! THE FOUR INSTALLED BROWSERS, asked on a loopback page (headless, 2026-09-17):
//!
//! ```text
//! browser              typeof navigator.connection   typeof NetworkInformation   effectiveType rtt downlink saveData
//! Chrome 150.0.7871.46 object                        function                    4g            100 1.5      false
//! Brave 151.0.7922.173 undefined ('connection' in    function                    -
//!                      navigator is false)
//! Edge 151.0.4129.93   object                        function                    4g            100 1.45     false
//! Opera 135 (151.0.7922.176) object                  function                    4g            200 1.55     false
//! ```
//!
//! So WHETHER the member exists is the capture's business (it is a `Navigator` row the installer joins
//! against the active `surface.json`, and Brave's capture does not list it), and WHAT it reports is the
//! persona's: `values.json`'s `nav.connection` block, read live through the kernel's
//! `spec::PersonaHandle` (the same `client_hints()` call `userAgentData` reads). No value here is a
//! literal of any browser. `rtt` and `downlink` are network ESTIMATES that move between two reads of
//! the same browser (Chrome answered 1.5 here where its capture recorded 1.3); the persona's recorded
//! estimate is the device's, and it is passed through Blink's own noise and bucketing.
//!
//! THE NOISE AND THE BUCKETS ARE CHROMIUM 150'S (`network_state_notifier.cc`, read 2026-10-09 -- this file
//! said 25 ms and 25 kbit/s with no noise): `RoundRtt` multiplies the estimate by `GetRandomMultiplier(host)`,
//! caps it at 3 s and rounds it to a multiple of 50 ms; `RoundMbps` multiplies, caps at 10 Mbit/s and rounds to
//! 50 kbit/s. The multiplier is `0.90 + ((hash(host) + salt) % 21) * 0.01`: `hash` the host string's Blink hash
//! (rapidhash, low 24 bits), `host` the realm's URL's (`NetworkInformation::Host`, `""` for a URL with none),
//! `salt` drawn once per renderer PROCESS from 1..=20. Site isolation runs the page and a cross-site frame in
//! two processes, so an interactive-widget challenge frame (a challenge host) and its page read two different noisy
//! values of one estimate in Chrome (the traced Chrome 150's frame: rtt 150, downlink 1.6), where this engine
//! answered the recorded 100 and 1.3 in every realm alike.
//!
//! `type` and `downlinkMax` are not rows: no capture in the tree records them on Linux desktop, and
//! Chrome 150 answers `undefined` for both.

use deno_core::v8;

use crate::binding::protos::str_val;
use crate::install::webidl::{brand, branded_getter_row};
use crate::install::MemberDef;

use super::navigator::client_hints;

/// Blink's `RoundRtt(host, rtt)`: the estimate times the host's multiplier (`TimeDelta * double`, truncated to
/// the microsecond), at most 3 s, rounded to the nearest multiple of 50 ms (`RoundToMultiple`: half up).
pub(crate) fn round_rtt(rtt_ms: u32, multiplier: f64) -> u32 {
    let us = ((rtt_ms as i64 * 1000) as f64 * multiplier) as i64;
    let us = us.min(3_000_000);
    ((us + 25_000) / 50_000 * 50_000 / 1000) as u32
}

/// Blink's `RoundMbps(host, downlink)`: the estimate in kbit/s times the host's multiplier, at most 10 Mbit/s,
/// rounded to the nearest 50 kbit/s (`std::round`: half away from zero), in Mbit/s.
pub(crate) fn round_mbps(downlink_mbps: f64, multiplier: f64) -> f64 {
    if !downlink_mbps.is_finite() || downlink_mbps <= 0.0 {
        return 0.0;
    }
    let kbps = (downlink_mbps * 1000.0 * multiplier).min(10_000.0);
    (kbps / 50.0).round() * 50.0 / 1000.0
}

/// Chromium's vendored rapidhash (`third_party/rapidhash/rapidhash.h`: `PlainHashReader`, `RAPID_SEED`,
/// `rapid_secret`), which `StringHasher` hashes a Latin-1 string's bytes with.
fn rapidhash(p: &[u8]) -> u64 {
    const SEED: u64 = 0xbdd8_9aa9_8270_4029;
    const S: [u64; 3] = [0x2d35_8dcc_aa6c_78a5, 0x8bb8_4b93_962e_acc9, 0x4b33_a62e_d433_d4a3];
    fn mul(a: u64, b: u64) -> (u64, u64) {
        let r = a as u128 * b as u128;
        (r as u64, (r >> 64) as u64)
    }
    fn mix(a: u64, b: u64) -> u64 {
        let (lo, hi) = mul(a, b);
        lo ^ hi
    }
    let r64 = |i: usize| u64::from_le_bytes(p[i..i + 8].try_into().unwrap());
    let r32 = |i: usize| u32::from_le_bytes(p[i..i + 4].try_into().unwrap()) as u64;
    let len = p.len();
    let mut seed = SEED ^ mix(SEED ^ S[0], S[1]) ^ len as u64;
    let (mut a, mut b);
    if len <= 16 {
        if len >= 4 {
            let last = len - 4;
            a = (r32(0) << 32) | r32(last);
            let delta = (len & 24) >> (len >> 3);
            b = (r32(delta) << 32) | r32(last - delta);
        } else if len > 0 {
            a = ((p[0] as u64) << 56) | ((p[len >> 1] as u64) << 32) | p[len - 1] as u64;
            b = 0;
        } else {
            a = 0;
            b = 0;
        }
    } else {
        let mut i = len;
        let mut o = 0;
        if i > 48 {
            let (mut see1, mut see2) = (seed, seed);
            while i >= 48 {
                seed = mix(r64(o) ^ S[0], r64(o + 8) ^ seed);
                see1 = mix(r64(o + 16) ^ S[1], r64(o + 24) ^ see1);
                see2 = mix(r64(o + 32) ^ S[2], r64(o + 40) ^ see2);
                o += 48;
                i -= 48;
            }
            seed ^= see1 ^ see2;
        }
        if i > 16 {
            seed = mix(r64(o) ^ S[2], r64(o + 8) ^ seed ^ S[1]);
            if i > 32 {
                seed = mix(r64(o + 16) ^ S[2], r64(o + 24) ^ seed);
            }
        }
        a = r64(o + i - 16);
        b = r64(o + i - 8);
    }
    a ^= S[1];
    b ^= seed;
    let (lo, hi) = mul(a, b);
    mix(lo ^ S[0] ^ len as u64, hi ^ S[1])
}

/// `StringImpl::GetHash` of an 8-bit string: rapidhash, its low 24 bits (the top 8 are StringImpl's flags),
/// never 0.
fn blink_string_hash(s: &str) -> u32 {
    let h = (rapidhash(s.as_bytes()) & 0x00ff_ffff) as u32;
    if h == 0 {
        0x0080_0000
    } else {
        h
    }
}

/// `NetworkStateNotifier::GetRandomMultiplier(host)` under a process's `salt`.
fn random_multiplier(host: &str, salt: u32) -> f64 {
    let hash = blink_string_hash(host).wrapping_add(salt);
    0.9 + (hash % 21) as f64 * 0.01
}

/// The renderer process's `randomization_salt_` (`base::RandIntInclusive(1, 20)`, once per process) for the
/// process running a realm of `site`: one draw per site per session, as site isolation gives each site its own.
fn process_salt(site: &str) -> u32 {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static SALTS: OnceLock<Mutex<HashMap<String, u32>>> = OnceLock::new();
    let mut m = SALTS.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap_or_else(|e| e.into_inner());
    *m.entry(site.to_string()).or_insert_with(|| {
        let mut b = [0u8; 4];
        let _ = getrandom::getrandom(&mut b);
        1 + u32::from_le_bytes(b) % 20
    })
}

/// The running realm's multiplier: its URL's host, and the salt of the process its site runs in -- a worker's
/// own URL (a `blob:` one has no host) in its creator's process (a blob's site is the origin inside it).
fn realm_multiplier(scope: &mut v8::PinScope) -> f64 {
    let worker = crate::core::workers::worker_global_scope::worker_href(scope);
    let (url, site_url) = if worker.is_empty() {
        crate::core::html::browsing_context::realm_process_urls(scope)
    } else {
        let inner = worker.strip_prefix("blob:").unwrap_or(&worker).to_string();
        (worker, inner)
    };
    let host = url::Url::parse(&url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
    let site = url::Url::parse(&site_url).ok().and_then(|u| transport::schemeful_site(&u)).unwrap_or(site_url);
    random_multiplier(&host, process_salt(&site))
}

fn cb_effective_type(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let c = client_hints(scope).connection;
    rv.set(str_val(scope, &c.effective_type));
}

/// Chromium's NQE default observations (`net/nqe/network_quality_estimator_params.cc`, `ObtainDefaultObservations`):
/// the estimate a fresh profile reports before its own traffic has moved it, per connection type -- here the types a
/// desktop's network change notifier reports (`AddressTrackerLinux::GetCurrentConnectionType`: ethernet, wifi, or
/// unknown), as `(HTTP RTT ms, downstream kbit/s)`.
const DESKTOP_NQE_DEFAULTS: [(u32, u32); 3] = [(90, 1456), (116, 2658), (115, 1961)];

/// THE ESTIMATE behind a persona's recorded `rtt` and `downlink`.
///
/// A capture records one READING -- Blink's noise over its renderer's estimate, for the capture page's host in its
/// process (see the module docs) -- and that reading, used as the estimate, was noised a second time here: the Chrome
/// 150 bundle's `1.3` (Chrome's 1456 kbit/s under the lowest multiplier, 0.90) read 1.15..1.45 in this engine where
/// Chrome reads 1.3..1.6 (a bot-detection script: korvus 1.4, Chrome 1.5). The estimate is the desktop NQE default whose readings
/// include the recorded pair; a pair no default explains (a session whose own traffic had moved its estimate) is its
/// own estimate, as before.
pub(crate) fn connection_estimate(rtt: u32, downlink: f64) -> (u32, f64) {
    let reads = |rtt0: u32, kbps: u32| {
        (0..21u32).any(|k| {
            let m = 0.9 + k as f64 * 0.01;
            round_rtt(rtt0, m) == rtt && (round_mbps(kbps as f64 / 1000.0, m) - downlink).abs() < 1e-9
        })
    };
    DESKTOP_NQE_DEFAULTS
        .iter()
        .find(|(r, k)| reads(*r, *k))
        .map(|(r, k)| (*r, *k as f64 / 1000.0))
        .unwrap_or((rtt, downlink))
}

fn cb_rtt(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let c = client_hints(scope).connection;
    let (rtt, _) = connection_estimate(c.rtt, c.downlink);
    let m = realm_multiplier(scope);
    rv.set(v8::Integer::new_from_unsigned(scope, round_rtt(rtt, m)).into());
}

fn cb_downlink(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let c = client_hints(scope).connection;
    let (_, downlink) = connection_estimate(c.rtt, c.downlink);
    let m = realm_multiplier(scope);
    rv.set(v8::Number::new(scope, round_mbps(downlink, m)).into());
}

fn cb_save_data(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let c = client_hints(scope).connection;
    rv.set(v8::Boolean::new(scope, c.save_data).into());
}

/// The body every non-handler row of [`NETWORKINFORMATION`] names. Each getter is branded: a foreign
/// receiver is Chrome's `Illegal invocation`, not the persona's value.
pub fn install_network_information(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "effectiveType" => branded_getter_row(scope, proto, member, cb_effective_type),
        "rtt" => branded_getter_row(scope, proto, member, cb_rtt),
        "downlink" => branded_getter_row(scope, proto, member, cb_downlink),
        "saveData" => branded_getter_row(scope, proto, member, cb_save_data),
        other => debug_assert!(false, "install_network_information: unexpected member {other}"),
    }
}

// Rows VERBATIM from the `NetworkInformation` block of `install/generated_registry.rs`, in the
// capture's order (`onchange`, `effectiveType`, `rtt`, `downlink`, `saveData` -- Chrome 150's
// `Object.getOwnPropertyNames(NetworkInformation.prototype)` is that order plus `constructor`).
// `[interface, both]`: every capture lists it in both realms, Brave's included.
webapi::interface! {
    "NetworkInformation" : "EventTarget" [interface, both] {
        ctor    "NetworkInformation"                  len 0 = stub,
        handler "onchange"                                  = event_handler,
        attr    "effectiveType" : Any     readonly          = install_network_information,
        attr    "rtt"           : Integer readonly          = install_network_information,
        attr    "downlink"      : Double  readonly          = install_network_information,
        attr    "saveData"      : Boolean readonly          = install_network_information
    }
}

/// `navigator.connection`: one `NetworkInformation` per realm (the lazy row memoizes it), on the
/// table's prototype, branded, with no own property. `null` in a realm whose table is not installed.
pub(crate) fn build_connection<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Value> {
    let Some(proto) = crate::binding::protos::shell_proto(scope, "NetworkInformation") else {
        return v8::null(scope).into();
    };
    let o = crate::binding::protos::mint_on(scope, proto.into());
    brand(scope, o, "NetworkInformation");
    o.into()
}

/// Create the interface -- called by the registry walk at the capture's own-global position of
/// `NetworkInformation` (`install::live::ORDERED_HAND`), in both realms. `EventTarget` exists by then
/// in both (the window mints it first thing in its bootstrap, the worker's bootstrap precedes its walk).
pub fn create_network_information<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    global: v8::Local<'s, v8::Object>,
    ledger: &mut crate::install::installer::InstallLedger,
) {
    use crate::install::installer::{self, Flavor};
    let et = crate::install::live::rows::parent_of(scope, "EventTarget");
    let built = installer::create_interface(scope, global, &NETWORKINFORMATION, "NetworkInformation", et, None, Flavor::Table, ledger);
    for g in &built.gaps {
        eprintln!("[korvus] network-information install gap: {}.{}", g.interface, g.member);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Chromium's own rapidhash over these strings: `third_party/rapidhash/rapidhash.h` from ~/chromium-150, built
    /// standalone (`base/` macros stubbed) and run 2026-10-09 -- every length path (0, 1-3, 4-16, 17-48, >48).
    #[test]
    fn rapidhash_is_chromiums() {
        for (s, h, masked) in [
            ("", 0x5a6e_f770_74eb_c84b_u64, 15452235_u32),
            ("a", 0xc113_2847_7bc0_f5d1, 12645841),
            ("ab", 0x5644_ac03_5e40_d569, 4248937),
            ("abc", 0x0347_080f_bf5f_cd81, 6278529),
            ("abcd", 0x056b_66b8_dc80_2bcc, 8399820),
            ("player.fm", 0xa12b_1e92_d6f7_ee0b, 16248331),
            ("127.0.0.1", 0x70cc_d221_6445_2ae6, 4532966),
            ("www.example.com", 0xbc95_c60e_658c_387d, 9189501),
            ("rateyourmusic.com", 0x16cd_923e_7f85_5fe6, 8740838),
            ("a-very-long-host-name-for-testing-the-48-byte-loop.example.co.uk", 0x7189_8713_c842_a741, 4368193),
        ] {
            assert_eq!(rapidhash(s.as_bytes()), h, "{s:?}");
            assert_eq!(blink_string_hash(s), masked, "{s:?}");
        }
    }

    /// `GetRandomMultiplier`: `0.9 + ((hash + salt) % 21) * 0.01` -- the reference run's buckets for salt 1 and 20.
    #[test]
    fn the_multiplier_is_the_hash_plus_the_salt_bucketed() {
        assert_eq!(random_multiplier("rateyourmusic.com", 1), 0.9 + 9.0 * 0.01);
        assert_eq!(random_multiplier("", 6), 0.9 + 0.0 * 0.01);
        for salt in 1..=20 {
            let m = random_multiplier("player.fm", salt);
            assert!((0.9..=1.1).contains(&m), "{m}");
        }
    }

    /// `RoundRtt` / `RoundMbps`: 50 ms and 50 kbit/s buckets, half up (`RoundToMultiple`) and half away from zero
    /// (`std::round`), the caps, and the multiplier applied before rounding.
    #[test]
    fn blink_bucketing_rounds_and_caps() {
        assert_eq!(round_rtt(100, 1.0), 100);
        assert_eq!(round_rtt(124, 1.0), 100);
        assert_eq!(round_rtt(125, 1.0), 150);
        assert_eq!(round_rtt(137, 1.0), 150);
        assert_eq!(round_rtt(9000, 1.0), 3000);
        assert_eq!(round_rtt(115, 1.09), 150);
        assert_eq!(round_rtt(115, 1.08), 100);
        assert!((round_mbps(1.3, 1.0) - 1.3).abs() < 1e-12);
        assert!((round_mbps(1.325, 1.0) - 1.35).abs() < 1e-12);
        assert!((round_mbps(1.324, 1.0) - 1.3).abs() < 1e-12);
        assert!((round_mbps(1.961, 1.03) - 2.0).abs() < 1e-12);
        assert_eq!(round_mbps(42.0, 1.0), 10.0);
        assert_eq!(round_mbps(-1.0, 1.0), 0.0);
    }

    /// The bundles' recorded pairs, and the estimate each is a reading of: Chrome 150's `100`/`1.3` and Edge 151's
    /// `100`/`1.55` are readings of the ethernet default (90 ms, 1456 kbit/s -- under 0.90 and 1.06); Opera 135's
    /// `200`/`1.75` is a reading of none, so it stands as recorded.
    #[test]
    fn a_recorded_reading_is_traced_back_to_its_estimate() {
        assert_eq!(connection_estimate(100, 1.3), (90, 1.456));
        assert_eq!(connection_estimate(100, 1.55), (90, 1.456));
        assert_eq!(connection_estimate(200, 1.75), (200, 1.75));
        // and every reading of that estimate is in Chrome's 1.3..1.6, rtt 100
        for k in 0..21 {
            let m = 0.9 + k as f64 * 0.01;
            let d = round_mbps(1.456, m);
            assert!((1.3 - 1e-9..=1.6 + 1e-9).contains(&d), "{m}: {d}");
            assert_eq!(round_rtt(90, m), 100);
        }
    }
}
