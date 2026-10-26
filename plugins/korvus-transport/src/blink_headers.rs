//! The order Blink hands a subresource's header map to the network stack.
//!
//! A request Blink builds -- `fetch()`, XHR, an element's load -- keeps its headers in an
//! `HTTPHeaderMap`: a WTF `HashMap` keyed case-insensitively. The network stack copies that map into
//! the request IN ITERATION ORDER, which is the order of the hash table's buckets, and appends its own
//! headers (`Accept`, `Sec-Fetch-*`, `Referer`, `Accept-Encoding`, ...) after them. So the wire order
//! of a page's own headers is a function of their NAMES, not of the order the page set them --
//! measured 2026-10-02 on Brave 151 and Chrome 150 (identical): `x-d, x-c, x-b, x-a` set in that order
//! goes out exactly as `x-a, x-b, x-c, x-d` does, and the client hints move between table sizes.
//!
//! What the map is, read from Chromium's source (`wtf/hash_table.h`, `wtf/text/case_folding_hash.h`,
//! `wtf/text/string_hasher.h`, `third_party/rapidhash/rapidhash.h`) and pinned by 50 measured orders
//! (`tools/realweb/seed/hdrorder` plus three discriminating rounds):
//!
//! * the hash is rapidhash over the name CASE-FOLDED TO UTF-16 (each Latin-1 byte widened to 16 bits),
//!   truncated to 32 bits and masked to 24 (`MaskTop8Bits`, 0 becomes `0x800000`);
//! * the table starts at 16 buckets, doubles when `keys * 2 >= size`, and probes triangularly
//!   (`i = (i + n) & mask` on the n-th collision); a rehash reinserts in old bucket order;
//! * a page's headers go in first, in the order set, then `Content-Type`, then the client hints in
//!   Blink's insertion order (`Sec-CH-UA`, `Sec-CH-UA-Mobile`, `Sec-CH-UA-Platform` with no `Accept-CH`;
//!   `client_hints::UA_BLINK_ORDER` with one) and `User-Agent` (the one insertion order the
//!   discriminating rounds left standing).

const SECRET: [u64; 3] = [0x2d35_8dcc_aa6c_78a5, 0x8bb8_4b93_962e_acc9, 0x4b33_a62e_d433_d4a3];
const RAPID_SEED: u64 = 0xbdd8_9aa9_8270_4029;

fn mum(a: u64, b: u64) -> (u64, u64) {
    let r = (a as u128) * (b as u128);
    (r as u64, (r >> 64) as u64)
}

fn mix(a: u64, b: u64) -> u64 {
    let (a, b) = mum(a, b);
    a ^ b
}

fn r64(p: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(p[i..i + 8].try_into().expect("8 bytes"))
}

fn r32(p: &[u8], i: usize) -> u64 {
    u32::from_le_bytes(p[i..i + 4].try_into().expect("4 bytes")) as u64
}

/// rapidhash v1 (`rapidhash_internal` with the plain reader) over `p`. `small` is what the reader
/// answers for an input of 1-3 bytes; the case-folding reader is only ever asked for 2 (one UTF-16
/// unit) and answers the folded unit itself.
fn rapidhash(p: &[u8], small: impl Fn(&[u8]) -> u64) -> u64 {
    let len = p.len();
    let mut seed = RAPID_SEED ^ mix(RAPID_SEED ^ SECRET[0], SECRET[1]) ^ len as u64;
    let (mut a, mut b);
    if len <= 16 {
        if len >= 4 {
            let plast = len - 4;
            a = (r32(p, 0) << 32) | r32(p, plast);
            let delta = (len & 24) >> (len >> 3);
            b = (r32(p, delta) << 32) | r32(p, plast - delta);
        } else if len > 0 {
            a = small(p);
            b = 0;
        } else {
            a = 0;
            b = 0;
        }
    } else {
        let mut i = len;
        let mut off = 0;
        if i > 48 {
            let (mut see1, mut see2) = (seed, seed);
            while i >= 48 {
                seed = mix(r64(p, off) ^ SECRET[0], r64(p, off + 8) ^ seed);
                see1 = mix(r64(p, off + 16) ^ SECRET[1], r64(p, off + 24) ^ see1);
                see2 = mix(r64(p, off + 32) ^ SECRET[2], r64(p, off + 40) ^ see2);
                off += 48;
                i -= 48;
            }
            seed ^= see1 ^ see2;
        }
        if i > 16 {
            seed = mix(r64(p, off) ^ SECRET[2], r64(p, off + 8) ^ seed ^ SECRET[1]);
            if i > 32 {
                seed = mix(r64(p, off + 16) ^ SECRET[2], r64(p, off + 24) ^ seed);
            }
        }
        a = r64(p, off + i - 16);
        b = r64(p, off + i - 8);
    }
    a ^= SECRET[1];
    b ^= seed;
    let (a, b) = mum(a, b);
    mix(a ^ SECRET[0] ^ len as u64, b ^ SECRET[1])
}

/// `DeprecatedCaseFoldingHash` of a header name: 24 bits, never 0.
pub(crate) fn name_hash(name: &str) -> u32 {
    let units: Vec<u8> = name.bytes().flat_map(|c| [c.to_ascii_lowercase(), 0]).collect();
    let h = rapidhash(&units, |p| p[0] as u64) as u32 & ((1 << 24) - 1);
    if h == 0 { 0x80_0000 } else { h }
}

/// The bucket order of a WTF `HashMap` after inserting `keys` (case-insensitively distinct, in order).
pub(crate) fn map_order(keys: &[String]) -> Vec<String> {
    fn insert(table: &mut [Option<(String, u32)>], key: String, h: u32) {
        let mask = table.len() - 1;
        let mut i = h as usize & mask;
        let mut n = 0;
        while table[i].is_some() {
            n += 1;
            i = (i + n) & mask;
        }
        table[i] = Some((key, h));
    }
    let mut table: Vec<Option<(String, u32)>> = vec![None; 16];
    let mut count = 0;
    for k in keys {
        if table.iter().flatten().any(|(e, _)| e.eq_ignore_ascii_case(k)) {
            continue;
        }
        insert(&mut table, k.clone(), name_hash(k));
        count += 1;
        if count * 2 >= table.len() {
            let old = std::mem::replace(&mut table, vec![None; 0]);
            table = vec![None; old.len() * 2];
            for (k, h) in old.into_iter().flatten() {
                insert(&mut table, k, h);
            }
        }
    }
    table.into_iter().flatten().map(|(k, _)| k).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The identity headers Blink adds to a request's map after the page's own, in its insertion order,
    /// when no `Accept-CH` asked for more (the transport builds this from `client_hints::UA_BLINK_ORDER`).
    const IDENTITY_IN_MAP: [&str; 4] = ["sec-ch-ua", "sec-ch-ua-mobile", "sec-ch-ua-platform", "user-agent"];

    fn order(names: &[&str]) -> Vec<String> {
        map_order(&names.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    /// Orders measured on the wire (Brave 151 = Chrome 150, 2026-10-02; `tools/realweb/seed/hdrorder`).
    #[test]
    fn the_map_order_is_the_measured_wire_order() {
        let id = IDENTITY_IN_MAP;
        let with = |pre: &[&'static str]| -> Vec<&'static str> { pre.iter().copied().chain(id.iter().copied()).collect() };
        assert_eq!(order(&id), ["sec-ch-ua-platform", "user-agent", "sec-ch-ua", "sec-ch-ua-mobile"]);
        assert_eq!(order(&with(&["x-a", "content-type"])), ["sec-ch-ua-platform", "user-agent", "sec-ch-ua", "content-type", "x-a", "sec-ch-ua-mobile"]);
        assert_eq!(
            order(&with(&["x-a", "x-b", "x-c", "x-d", "x-e", "content-type"])),
            ["x-d", "sec-ch-ua-platform", "x-c", "sec-ch-ua", "sec-ch-ua-mobile", "x-a", "x-e", "user-agent", "content-type", "x-b"]
        );
        // set order does not matter: [d, c, b, a] is [a, b, c, d]
        assert_eq!(order(&with(&["x-d", "x-c", "x-b", "x-a", "content-type"])), order(&with(&["x-a", "x-b", "x-c", "x-d", "content-type"])));
        // a media request's Accept-Encoding is in the map
        assert_eq!(order(&with(&["accept-encoding"])), ["sec-ch-ua-platform", "accept-encoding", "user-agent", "sec-ch-ua", "sec-ch-ua-mobile"]);
        // so is the `Origin` a CORS-configured load's loader set (Chrome 150 over h2)
        assert_eq!(order(&with(&["origin"])), ["origin", "sec-ch-ua-platform", "user-agent", "sec-ch-ua", "sec-ch-ua-mobile"]);
        // case-insensitive keys
        assert_eq!(name_hash("Content-Type"), name_hash("content-type"));
    }

    /// With an origin's `Accept-CH` the high-entropy hints join the map in Blink's insertion order
    /// (`client_hints::UA_BLINK_ORDER`) and the table grows past 16 buckets. The orders Chrome 150 put on
    /// the wire: nih.gov's challenge script and its XHR POST (a recorded live visit,
    /// nine hints), and the loopback fixture's XHR with all eleven (`tests/fixtures/client-hints/`).
    #[test]
    fn the_high_entropy_hints_take_the_measured_wire_order() {
        let nine = [
            "sec-ch-ua", "sec-ch-ua-mobile", "sec-ch-ua-arch", "sec-ch-ua-platform", "sec-ch-ua-platform-version",
            "sec-ch-ua-model", "sec-ch-ua-full-version", "sec-ch-ua-full-version-list", "sec-ch-ua-bitness",
        ];
        let with = |pre: &[&'static str], hints: &[&'static str]| -> Vec<&'static str> {
            pre.iter().copied().chain(hints.iter().copied()).chain(std::iter::once("user-agent")).collect()
        };
        assert_eq!(
            order(&with(&[], &nine)),
            [
                "sec-ch-ua-full-version-list", "sec-ch-ua-platform", "sec-ch-ua", "sec-ch-ua-bitness", "sec-ch-ua-model",
                "sec-ch-ua-mobile", "sec-ch-ua-arch", "sec-ch-ua-full-version", "user-agent", "sec-ch-ua-platform-version",
            ]
        );
        assert_eq!(
            order(&with(&["cf-chl", "cf-chl-ra", "content-type"], &nine)),
            [
                "sec-ch-ua-full-version-list", "sec-ch-ua-platform", "sec-ch-ua", "sec-ch-ua-bitness", "cf-chl-ra",
                "sec-ch-ua-mobile", "sec-ch-ua-model", "sec-ch-ua-arch", "sec-ch-ua-full-version", "user-agent",
                "content-type", "cf-chl", "sec-ch-ua-platform-version",
            ]
        );
        let eleven: Vec<&'static str> = nine.iter().copied().chain(["sec-ch-ua-wow64", "sec-ch-ua-form-factors"]).collect();
        assert_eq!(
            order(&with(&["x-test", "content-type"], &eleven)),
            [
                "x-test", "sec-ch-ua-platform", "sec-ch-ua-full-version-list", "sec-ch-ua", "sec-ch-ua-bitness",
                "sec-ch-ua-model", "sec-ch-ua-mobile", "sec-ch-ua-form-factors", "sec-ch-ua-wow64", "sec-ch-ua-arch",
                "sec-ch-ua-full-version", "user-agent", "content-type", "sec-ch-ua-platform-version",
            ]
        );
    }
}
