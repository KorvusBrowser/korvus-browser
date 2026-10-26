//! Brave's fingerprint farbling, the runtime half.
//!
//! `bundles/<brave>/farbling.json` CAPTURES the policy; this module APPLIES it. Until now korvus served
//! the one static captured scalar (`navigator.deviceMemory` = 32, `hardwareConcurrency` = 12), where
//! shields-default Brave **redraws each farbled signal per (session, eTLD+1 host)** -- so two origins see
//! two values and a detector that compares across origins sees a constant where a real Brave varies. That
//! divergence is what flags korvus at `deviceandbrowserinfo` while every wire and JS axis is byte-identical
//! to Brave (STATE C13). This is faithful-browser behaviour, not a solver: it reproduces what Brave itself
//! does, from Brave's own captured policy.
//!
//! The model, measured in `farbling.json` `scope` (2026-09-19):
//!   * a signal is redrawn **per (session, host string)** and per nothing else -- NOT per page load, per
//!     tab, or per read. Same origin in one session is stable; a second origin or a new session redraws.
//!   * keyed on the origin's **host string as written** (`localhost` and `127.0.0.1` draw differently),
//!     not the resolved address.
//!   * `navigator.hardwareConcurrency`: uniform over the integers `[2, N]`, N = the host's unfarbled core
//!     count (12 here). `navigator.deviceMemory`: uniform over the fixed set `{4, 8, 16, 32}`.
//!
//! The draw is a deterministic function of `(session_seed, host)`: the exact value per origin need not
//! match a given Brave instance (Brave's own seed is random per session), only the SHAPE -- stable per
//! `(session, host)`, uniform over the support across hosts -- so korvus is indistinguishable from "a Brave
//! with some seed". The session seed is a random `u64` made once per browsing session ([`SessionSeed`]).

/// A browsing session's farbling seed: one `u64`, made once and carried for the life of the session,
/// exactly as Brave keeps one seed per session. All of a session's per-origin draws derive from it. This
/// module is pure (seed in, value out); the random seed is minted at the engine layer, which owns an RNG,
/// and handed in -- so `korvus-persona` stays dependency-free.
#[derive(Debug, Clone, Copy)]
pub struct SessionSeed(pub u64);

/// A stable 64-bit key for `(seed, host)`. FNV-1a over the seed's bytes then the host's, which is enough:
/// the draw only needs determinism and a good spread across hosts, not cryptographic strength (Brave's own
/// is a non-cryptographic PRNG seeded per session).
fn mix(seed: u64, host: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in seed.to_le_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    for b in host.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    // A final splitmix64 step so low bits are well mixed (FNV's low bits move little per added byte).
    h = (h ^ (h >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    h = (h ^ (h >> 27)).wrapping_mul(0x94d049bb133111eb);
    h ^ (h >> 31)
}

/// A uniform draw from `[lo, hi]` inclusive, keyed on `(seed, host)` -- `navigator.hardwareConcurrency`'s
/// policy with `lo = 2`, `hi = N`.
pub fn uniform_range(seed: SessionSeed, host: &str, lo: i64, hi: i64) -> i64 {
    if hi <= lo {
        return lo;
    }
    let span = (hi - lo + 1) as u64;
    lo + (mix(seed.0, host) % span) as i64
}

/// A uniform draw from an explicit set, keyed on `(seed, host)` -- `navigator.deviceMemory`'s policy with
/// the set `{4, 8, 16, 32}`. An empty set returns `0.0` (the persona-less default elsewhere).
pub fn uniform_set(seed: SessionSeed, host: &str, set: &[f64]) -> f64 {
    if set.is_empty() {
        return 0.0;
    }
    set[(mix(seed.0, host) as usize) % set.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    const DM_SET: [f64; 4] = [4.0, 8.0, 16.0, 32.0];
    const HWC_N: i64 = 12;

    #[test]
    fn stable_per_session_and_host() {
        // The whole point of "per (session, host) and per nothing else": the same pair ALWAYS draws the
        // same value, so two reads on one page -- and a second tab of the same origin -- agree.
        let s = SessionSeed(0x1234_5678_9abc_def0);
        for host in ["example.com", "deviceandbrowserinfo.com", "127.0.0.1", "localhost"] {
            let a = uniform_set(s, host, &DM_SET);
            let b = uniform_set(s, host, &DM_SET);
            assert_eq!(a, b, "deviceMemory must be stable for ({s:?}, {host})");
            let c = uniform_range(s, host, 2, HWC_N);
            let d = uniform_range(s, host, 2, HWC_N);
            assert_eq!(c, d, "hardwareConcurrency must be stable for ({s:?}, {host})");
        }
    }

    #[test]
    fn localhost_and_loopback_draw_independently() {
        // `scope.keyed_on`: the host STRING, not the resolved address -- `localhost` and `127.0.0.1` are
        // one socket but must draw independently. Over the seeds where they would collide by chance this
        // is not provable per-seed; assert the KEYS differ, which is what makes the draws independent.
        let s = SessionSeed(1);
        assert_ne!(mix(s.0, "localhost"), mix(s.0, "127.0.0.1"));
    }

    #[test]
    fn range_draw_stays_in_support() {
        let s = SessionSeed(42);
        for i in 0..5000u64 {
            let host = format!("h{i}.example");
            let v = uniform_range(s, &host, 2, HWC_N);
            assert!((2..=HWC_N).contains(&v), "hwc {v} out of [2,{HWC_N}] for {host}");
            let m = uniform_set(s, &host, &DM_SET);
            assert!(DM_SET.contains(&m), "deviceMemory {m} not in the captured set for {host}");
        }
    }

    #[test]
    fn spreads_over_the_support_across_hosts() {
        // Brave's distribution is ~uniform over the support (farbling.json chi2_vs_uniform 0.684 for
        // deviceMemory, 16.9 for hwc over 11 bins). We only need every value of the support to be REACHED
        // across many hosts in one session -- a constant (today's bug) reaches exactly one.
        let s = SessionSeed(0xdead_beef);
        let mut dm = std::collections::BTreeSet::new();
        let mut hwc = std::collections::BTreeSet::new();
        for i in 0..4000u64 {
            let host = format!("site-{i}.test");
            dm.insert(uniform_set(s, &host, &DM_SET) as i64);
            hwc.insert(uniform_range(s, &host, 2, HWC_N));
        }
        assert_eq!(dm.len(), DM_SET.len(), "every deviceMemory value must be reached across hosts");
        assert_eq!(hwc.len(), (HWC_N - 2 + 1) as usize, "every hwc value in [2,12] must be reached");
    }

    #[test]
    fn a_different_session_redraws() {
        // `differs_across_sessions` in the evidence: a new session seed generally gives a different value.
        // Not guaranteed per host, so assert it holds for a clear majority across many hosts.
        let s1 = SessionSeed(1);
        let s2 = SessionSeed(2);
        let mut differ = 0;
        for i in 0..200u64 {
            let host = format!("x{i}.example");
            if uniform_set(s1, &host, &DM_SET) != uniform_set(s2, &host, &DM_SET) {
                differ += 1;
            }
        }
        assert!(differ > 120, "a new session should redraw most origins' deviceMemory; got {differ}/200");
    }
}
