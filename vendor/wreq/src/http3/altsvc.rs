//! KORVUS PATCH (H3): where Chrome 150 learns that an origin speaks HTTP/3, and when it stops trying.
//!
//!  * [`parse`]: `SpdyAltSvcWireFormat::ParseHeaderFieldValue` (`http2/core/spdy_alt_svc_wire_format.cc`) -- `clear`,
//!    percent-decoded protocol ids, a quoted `host:port` (port mandatory, host may be empty), `ma` (default 86400 s),
//!    `v` only for non-`hq` entries; every other parameter (`persist` included) skipped.
//!  * [`AltSvcCache::store`]: `HttpStreamFactory::ProcessAlternativeServices` (`net/http/http_stream_factory.cc:130-155`)
//!    -- a response's Alt-Svc REPLACES the origin's list; only `h3` (the ALPN of RFC v1, the one version Chrome 150
//!    supports, `quic_versions.cc:510-519`) and `h2` are kept, `h2` never used (`enable_http2_alternative_service =
//!    false`); a port outside 0..=65535 is dropped (`IsPortValid`); expiry is now + `ma`. The cache is keyed by
//!    origin AND network anonymization key (`http_server_properties.cc:150-151`, `PartitionByDefault`).
//!  * [`AltSvcCache::quic_for`]: `JobController::GetAlternativeServiceInfoInternal`
//!    (`http_stream_factory_job_controller.cc:1383-1505`) -- https only, the first unexpired, unbroken `h3` entry; an
//!    alternative port >= 1024 for an origin port < 1024 is skipped (`enable_user_alternate_protocol_ports = false`);
//!    another host is allowed (`allow_remote_alt_svc = true`).
//!  * [`Broken`]: `BrokenAlternativeServices` (`net/http/broken_alternative_services.cc:20-56`) -- broken for
//!    300 s * 2^n (n capped at 18), at most two days.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

/// One Alt-Svc entry as the header spelled it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) protocol: String,
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) max_age: u32,
}

fn skip_ws(b: &[u8], at: &mut usize, end: usize) {
    while *at < end && (b[*at] == b' ' || b[*at] == b'\t') {
        *at += 1;
    }
}

fn parse_positive<T: TryFrom<u64>>(s: &[u8]) -> Option<T> {
    if s.is_empty() || !s.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let mut v: u64 = 0;
    for d in s {
        v = v.checked_mul(10)?.checked_add(u64::from(d - b'0'))?;
    }
    T::try_from(v).ok()
}

fn percent_decode(s: &[u8]) -> Option<String> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] != b'%' {
            out.push(s[i]);
            i += 1;
            continue;
        }
        let hi = (*s.get(i + 1)? as char).to_digit(16)?;
        let lo = (*s.get(i + 2)? as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
        i += 3;
    }
    String::from_utf8(out).ok()
}

fn parse_authority(s: &[u8]) -> Option<(String, u16)> {
    let mut host = Vec::new();
    let mut i = 0;
    if s.first() == Some(&b'[') {
        while i < s.len() && s[i] != b']' {
            if s[i] == b'"' {
                return None;
            }
            host.push(s[i]);
            i += 1;
        }
        if i == s.len() {
            return None;
        }
        host.push(b']');
        i += 1;
    } else {
        while i < s.len() && s[i] != b':' {
            if s[i] == b'"' {
                return None;
            }
            if s[i] == b'\\' {
                i += 1;
                if i == s.len() {
                    return None;
                }
            }
            host.push(s[i]);
            i += 1;
        }
    }
    if i == s.len() || s[i] != b':' {
        return None;
    }
    let port = parse_positive::<u16>(&s[i + 1..])?;
    Some((String::from_utf8(host).ok()?, port))
}

/// Parse an `Alt-Svc` field value: `Some(vec![])` for `clear`, `None` for a value Chrome rejects whole.
pub(crate) fn parse(value: &str) -> Option<Vec<Entry>> {
    let b = value.as_bytes();
    if b.is_empty() {
        return None;
    }
    if value == "clear" {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    let mut c = 0;
    while c < b.len() {
        let eq = c + b[c..].iter().position(|x| *x == b'=')?;
        if eq == c {
            return None;
        }
        let protocol = percent_decode(&b[c..eq])?;
        let is_hq = protocol == "hq";
        c = eq + 1;
        if c >= b.len() || b[c] != b'"' {
            return None;
        }
        c += 1;
        let auth_begin = c;
        while c < b.len() && b[c] != b'"' {
            if b[c] == b'\\' {
                c += 1;
                if c >= b.len() {
                    return None;
                }
            }
            c += 1;
        }
        if c == auth_begin || c >= b.len() {
            return None;
        }
        let (host, port) = parse_authority(&b[auth_begin..c])?;
        c += 1;
        let mut max_age: u32 = 86400;
        let mut params_end = c + b[c..].iter().position(|x| *x == b',').unwrap_or(b.len() - c);
        while c != params_end {
            skip_ws(b, &mut c, params_end);
            if c == params_end {
                break;
            }
            if b[c] != b';' {
                return None;
            }
            c += 1;
            skip_ws(b, &mut c, params_end);
            if c == params_end {
                break;
            }
            let mut name = String::new();
            while c != params_end && b[c] != b'=' && b[c] != b' ' && b[c] != b'\t' {
                name.push(b[c].to_ascii_lowercase() as char);
                c += 1;
            }
            skip_ws(b, &mut c, params_end);
            if c == params_end || b[c] != b'=' {
                return None;
            }
            c += 1;
            skip_ws(b, &mut c, params_end);
            let vbegin = c;
            while c != params_end && b[c] != b';' && b[c] != b' ' && b[c] != b'\t' {
                c += 1;
            }
            if c == vbegin {
                return None;
            }
            if name == "ma" {
                max_age = parse_positive::<u32>(&b[vbegin..c])?;
            } else if !is_hq && name == "v" {
                // A quoted list of versions; it may span commas, so the parameter list's end moves past it.
                if b[vbegin] != b'"' {
                    return None;
                }
                let close = vbegin + 1 + b[vbegin + 1..].iter().position(|x| *x == b'"')?;
                c = close + 1;
                params_end = c + b[c..].iter().position(|x| *x == b',').unwrap_or(b.len() - c);
                let list = &b[vbegin + 1..close];
                for v in list.split(|x| *x == b',') {
                    parse_positive::<u16>(v)?;
                }
            }
        }
        out.push(Entry { protocol, host, port, max_age });
        while c < b.len() && (b[c] == b' ' || b[c] == b'\t' || b[c] == b',') {
            c += 1;
        }
    }
    Some(out)
}

/// A usable alternative: an `h3` endpoint and when the advertisement expires.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Alternative {
    pub(crate) host: String,
    pub(crate) port: u16,
}

#[derive(Debug, Clone)]
struct Stored {
    h3: Vec<(Alternative, Instant)>,
}

/// The broken-alternative-service record: `BrokenAlternativeServices`.
#[derive(Default)]
pub(crate) struct Broken {
    map: HashMap<(Alternative, String), (Instant, u32)>,
}

const BROKEN_INITIAL: Duration = Duration::from_secs(300);
const BROKEN_MAX: Duration = Duration::from_secs(2 * 24 * 3600);
const BROKEN_MAX_SHIFT: u32 = 18;

impl Broken {
    pub(crate) fn is_broken(&self, alt: &Alternative, partition: &str, now: Instant) -> bool {
        self.map.get(&(alt.clone(), partition.to_string())).is_some_and(|(until, _)| *until > now)
    }

    /// Mark `alt` broken for this partition, for `300 s * 2^count` (`:24-56`).
    pub(crate) fn mark(&mut self, alt: &Alternative, partition: &str, now: Instant) {
        let e = self.map.entry((alt.clone(), partition.to_string())).or_insert((now, 0));
        let shift = e.1.min(BROKEN_MAX_SHIFT);
        let delay = BROKEN_INITIAL.saturating_mul(1u32 << shift).min(BROKEN_MAX);
        e.0 = now + delay;
        e.1 = e.1.saturating_add(1);
    }

    /// A confirmed handshake clears the record (`ConfirmAlternativeService`).
    pub(crate) fn confirm(&mut self, alt: &Alternative, partition: &str) {
        self.map.remove(&(alt.clone(), partition.to_string()));
    }
}

/// The Alt-Svc cache: origin (scheme-host-port) x partition -> the stored list.
#[derive(Default)]
pub(crate) struct AltSvcCache {
    map: Mutex<HashMap<(String, String), Stored>>,
    pub(crate) broken: Mutex<Broken>,
}

impl AltSvcCache {
    /// A response from `origin` (an `https` origin over a connection whose certificate verified) carried `value`.
    pub(crate) fn store(&self, origin: &str, partition: &str, value: &str, now: Instant) {
        let Some(entries) = parse(value) else { return };
        let h3: Vec<(Alternative, Instant)> = entries
            .into_iter()
            .filter(|e| e.protocol == "h3")
            .map(|e| (Alternative { host: e.host, port: e.port }, now + Duration::from_secs(u64::from(e.max_age))))
            .collect();
        let Ok(mut map) = self.map.lock() else { return };
        let key = (origin.to_string(), partition.to_string());
        if h3.is_empty() {
            // `clear`, or a list with no QUIC entry: the origin's QUIC alternatives are replaced by nothing.
            map.remove(&key);
        } else {
            map.insert(key, Stored { h3 });
        }
    }

    /// The QUIC alternative to race for a request to `host:port` (an `https` origin) in this partition.
    pub(crate) fn quic_for(&self, origin: &str, host: &str, port: u16, partition: &str, now: Instant) -> Option<Alternative> {
        let map = self.map.lock().ok()?;
        let stored = map.get(&(origin.to_string(), partition.to_string()))?;
        let broken = self.broken.lock().ok()?;
        stored.h3.iter().find_map(|(alt, expires)| {
            if *expires <= now {
                return None;
            }
            let alt = Alternative {
                host: if alt.host.is_empty() { host.to_string() } else { alt.host.clone() },
                port: alt.port,
            };
            if alt.port >= 1024 && port < 1024 {
                return None;
            }
            if broken.is_broken(&alt, partition, now) {
                return None;
            }
            Some(alt)
        })
    }
}
