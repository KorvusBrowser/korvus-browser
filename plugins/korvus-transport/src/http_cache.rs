//! THE HTTP CACHE: Chrome's, for the session, in memory, partitioned.
//!
//! This engine had none ("every row is a network hit"), so a response was fetched again every time it was asked
//! for. Stock Chrome 150's NetLog of a player.fm load (a bot challenge page): the browser fetched
//! `/favicon.ico` twice, 148 ms apart; the first answer said `cache-control: max-age=300` with `age: 3178`, stale on
//! arrival, so the second went out conditional -- `if-modified-since` from the stored `last-modified`, ahead of
//! `priority` -- and came back `304`. This engine sent the second unconditional and downloaded it again: a request
//! sequence the site's edge sees, and that no Chrome sends.
//!
//! What is Chrome's here, from `net/http` (Chromium 150):
//! * the KEY: the request's network isolation key and its URL without the fragment (`HttpCache::GenerateCacheKey`,
//!   a nested document's own entry kept apart);
//! * FRESHNESS (`HttpResponseHeaders::GetFreshnessLifetimes`): none for `no-cache`/`no-store`/`Pragma: no-cache`;
//!   else `max-age`, else `Expires - Date`, else a tenth of `Date - Last-Modified` for 200/203/206, else forever for
//!   300/301/308/410, else none; `must-revalidate`;
//! * AGE (`GetCurrentAge`): `max(response_time - Date, Age + response_delay) + resident_time`;
//! * VALIDATION (`ConditionalizeRequest`): a stale answer with an `ETag` or `Last-Modified` goes out with
//!   `If-None-Match` then `If-Modified-Since`; a `304` updates the stored headers (`HttpResponseHeaders::Update`, its
//!   non-updated list) and the stored answer is served; a request whose `Vary`ing headers differ is validated too;
//! * STORAGE: a `GET`'s answer unless `no-store` (either side), `Vary: *` or `206`; never its `Set-Cookie`
//!   (`PERSIST_SANS_COOKIES`);
//! * the request's CACHE MODE (Fetch, as Blink maps it to load flags): `no-store` neither reads nor writes,
//!   `reload` writes only, `no-cache` always validates, `force-cache` takes any stored answer, `only-if-cached`
//!   takes one or fails; a request carrying its own `If-*` header behaves as `no-store`.
//!
//! Redirect answers are not stored (a stored one would have to be followed from here); nothing is written to disk
//! (a fresh profile's cache is what every comparison starts from).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// One stored answer.
#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub status: u16,
    /// The answer's headers, `Set-Cookie` removed.
    pub headers: Vec<(String, String)>,
    /// The body with its content coding removed, as the page reads it.
    pub body: Vec<u8>,
    /// The body's size on the wire (`encodedBodySize`).
    pub encoded_size: u64,
    /// When the request that brought it was sent and its answer arrived (seconds since the epoch, as `Date` is).
    pub request_time: f64,
    pub response_time: f64,
    /// The request's values of the headers the answer `Vary`s on (`None`: the request had none).
    pub vary: Vec<(String, Option<String>)>,
    /// The protocol it came over (`nextHopProtocol` when it is served as stored).
    pub version: wreq::Version,
}

/// What a request may do with the cache (Fetch's cache mode, as Blink's load flags carry it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Mode {
    pub read: bool,
    pub write: bool,
    /// `no-cache`: a stored answer is always validated.
    pub always_validate: bool,
    /// `force-cache` / `only-if-cached`: a stored answer is taken as it is.
    pub skip_validation: bool,
    /// `only-if-cached`: no stored answer is an error, not a network fetch.
    pub only_if_cached: bool,
}

/// DevTools' `Network.setCacheDisabled`: while set, every request reads nothing from the cache and still
/// stores its answer (Chrome's `LOAD_BYPASS_CACHE`), and goes out with `Pragma: no-cache` and `Cache-Control:
/// no-cache` -- Chrome 150 put both right after `Connection` on a `fetch()`, a `force-cache` one included, and neither
/// once the flag was cleared (`chrome150-a1s3c.json`).
static BYPASS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Set or clear the DevTools bypass ([`BYPASS`]).
pub fn set_bypass(on: bool) {
    BYPASS.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Is the DevTools bypass set?
pub(crate) fn bypassed() -> bool {
    BYPASS.load(std::sync::atomic::Ordering::Relaxed)
}

/// The cache mode of a request with this `RequestInit.cache` and these author headers.
pub(crate) fn mode(cache: Option<&str>, headers: &[(String, String)]) -> Mode {
    if bypassed() {
        return Mode { read: false, write: true, always_validate: false, skip_validation: false, only_if_cached: false };
    }
    let conditional = headers.iter().any(|(k, _)| {
        ["if-modified-since", "if-none-match", "if-unmodified-since", "if-match", "if-range"].iter().any(|n| k.eq_ignore_ascii_case(n))
    });
    let cache = match cache {
        Some("default") | None if conditional => "no-store",
        Some(c) => c,
        None => "default",
    };
    let base = Mode { read: true, write: true, always_validate: false, skip_validation: false, only_if_cached: false };
    match cache {
        "no-store" => Mode { read: false, write: false, ..base },
        "reload" => Mode { read: false, ..base },
        "no-cache" => Mode { always_validate: true, ..base },
        "force-cache" => Mode { skip_validation: true, ..base },
        "only-if-cached" => Mode { skip_validation: true, only_if_cached: true, ..base },
        _ => base,
    }
}

/// What a stored answer is good for, for this request now.
pub(crate) enum Use {
    /// Serve it as it is.
    Fresh(Entry),
    /// Ask the server with these validators; on `304`, serve it ([`refresh`]).
    Validate(Vec<(&'static str, String)>),
    /// Fetch from the network (nothing stored, or nothing to validate with).
    Miss,
}

/// Chrome's in-memory cache budget (`MemBackendImpl::Init`): 2% of physical memory, capped at 50 MB -- the cap on any
/// host with more than 2.5 GB, which is every host this engine runs on.
const MAX_BYTES: usize = 50 * 1024 * 1024;

/// The budget this process runs with. Chrome's 50 MB by default; `KORVUS_HTTP_CACHE_MAX=<MB>` (`korvus serve
/// --http-cache-max=N`) lowers it so a scrape at high concurrency spends less memory per page, and `0` turns the
/// cache off (nothing is stored). It is never raised above Chrome's 50 MB: a larger-than-Chrome cache would be its
/// own tell, and the point of the flag is to spend less, not more. Read once.
fn budget() -> usize {
    match std::env::var("KORVUS_HTTP_CACHE_MAX").ok().and_then(|v| v.trim().parse::<usize>().ok()) {
        Some(mb) => (mb * 1024 * 1024).min(MAX_BYTES),
        None => MAX_BYTES,
    }
}

/// The stored answers, BOUNDED as Chrome's memory backend is: the least recently used answer is evicted first
/// (`MemBackendImpl::EvictIfNeeded`, its LRU list), and an answer larger than an eighth of the budget is not stored
/// at all (`MaxFileSize`). This cache had no bound and nothing ever left it but a `no-store` or an unsafe method, so a
/// long-lived process kept every body it had ever cached.
pub(crate) struct Store {
    entries: HashMap<String, (Entry, u64)>,
    /// Last use -> key, oldest first.
    lru: std::collections::BTreeMap<u64, String>,
    bytes: usize,
    clock: u64,
    budget: usize,
}

impl Store {
    pub(crate) fn new(budget: usize) -> Self {
        Store { entries: HashMap::new(), lru: std::collections::BTreeMap::new(), bytes: 0, clock: 0, budget }
    }

    /// What an entry costs against the budget: its body, its headers, its recorded `Vary` and its key.
    fn size_of(key: &str, e: &Entry) -> usize {
        key.len()
            + e.body.len()
            + e.headers.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>()
            + e.vary.iter().map(|(k, v)| k.len() + v.as_ref().map_or(0, String::len)).sum::<usize>()
    }

    /// The answer under `key`, which is now the most recently used.
    fn get(&mut self, key: &str) -> Option<Entry> {
        let (e, tick) = self.entries.get_mut(key)?;
        self.lru.remove(tick);
        self.clock += 1;
        *tick = self.clock;
        self.lru.insert(self.clock, key.to_string());
        Some(e.clone())
    }

    fn remove(&mut self, key: &str) {
        if let Some((e, tick)) = self.entries.remove(key) {
            self.lru.remove(&tick);
            self.bytes -= Self::size_of(key, &e);
        }
    }

    /// Store `e` under `key`, replacing what was there, then evict the least recently used until the budget holds.
    fn insert(&mut self, key: &str, e: Entry) {
        self.remove(key);
        let size = Self::size_of(key, &e);
        if size > self.budget / 8 {
            return;
        }
        self.clock += 1;
        self.bytes += size;
        self.lru.insert(self.clock, key.to_string());
        self.entries.insert(key.to_string(), (e, self.clock));
        while self.bytes > self.budget {
            let Some((_, oldest)) = self.lru.pop_first() else { break };
            if let Some((e, _)) = self.entries.remove(&oldest) {
                self.bytes -= Self::size_of(&oldest, &e);
            }
        }
    }
}

fn store() -> &'static Mutex<Store> {
    static CACHE: OnceLock<Mutex<Store>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Store::new(budget())))
}

/// The wall clock, as `Date` reads it.
pub(crate) fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// The cache key: the partition (the request's network isolation key) and the URL without its fragment.
pub(crate) fn key(partition: &str, url: &url::Url) -> String {
    let mut u = url.clone();
    u.set_fragment(None);
    format!("{partition} {u}")
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

/// Every `Cache-Control` directive of `headers`, lower-cased, across all its fields.
fn directives(headers: &[(String, String)]) -> Vec<String> {
    headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("cache-control"))
        .flat_map(|(_, v)| v.split(','))
        .map(|d| d.trim().to_ascii_lowercase())
        .filter(|d| !d.is_empty())
        .collect()
}

fn seconds_of(directive: &str, name: &str) -> Option<f64> {
    let rest = directive.strip_prefix(name)?.trim_start().strip_prefix('=')?.trim().trim_matches('"');
    rest.parse::<u64>().ok().map(|s| s as f64)
}

/// `GetFreshnessLifetimes`: (freshness, staleness) in seconds.
fn lifetimes(status: u16, headers: &[(String, String)], response_time: f64) -> (f64, f64) {
    let d = directives(headers);
    if d.iter().any(|x| x == "no-cache" || x == "no-store") || header(headers, "pragma").is_some_and(|p| p.split(',').any(|t| t.trim().eq_ignore_ascii_case("no-cache"))) {
        return (0.0, 0.0);
    }
    let must_revalidate = d.iter().any(|x| x == "must-revalidate");
    let max_age = d.iter().find_map(|x| seconds_of(x, "max-age"));
    let swr = d.iter().find_map(|x| seconds_of(x, "stale-while-revalidate"));
    let staleness = if must_revalidate { 0.0 } else { swr.unwrap_or(0.0) };
    if let Some(m) = max_age {
        return (m, staleness);
    }
    let date = header(headers, "date").and_then(parse_http_date).unwrap_or(response_time);
    if let Some(expires) = header(headers, "expires") {
        // an unparseable `Expires` is in the past (RFC 9111 4.2.1)
        return match parse_http_date(expires) {
            Some(e) if e > date => (e - date, staleness),
            _ => (0.0, staleness),
        };
    }
    if matches!(status, 200 | 203 | 206) && !must_revalidate {
        if let Some(lm) = header(headers, "last-modified").and_then(parse_http_date) {
            if lm <= date {
                return ((date - lm) / 10.0, staleness);
            }
        }
    }
    if matches!(status, 300 | 301 | 308 | 410) {
        return (f64::INFINITY, 0.0);
    }
    (0.0, staleness)
}

/// `GetCurrentAge`, in seconds.
fn current_age(e: &Entry, now: f64) -> f64 {
    let date = header(&e.headers, "date").and_then(parse_http_date).unwrap_or(e.response_time);
    let age = header(&e.headers, "age").and_then(|a| a.trim().parse::<u64>().ok()).unwrap_or(0) as f64;
    let apparent = (e.response_time - date).max(0.0);
    let corrected = age + (e.response_time - e.request_time);
    apparent.max(corrected) + (now - e.response_time)
}

/// `IsResponseConditionalizable`: the validators a stale answer can be asked about, `If-None-Match` first.
fn validators(e: &Entry) -> Vec<(&'static str, String)> {
    if !matches!(e.status, 200 | 203) {
        return Vec::new();
    }
    let mut v = Vec::new();
    if let Some(etag) = header(&e.headers, "etag").filter(|t| !t.is_empty()) {
        v.push(("if-none-match", etag.to_string()));
    }
    if let Some(lm) = header(&e.headers, "last-modified").filter(|t| !t.is_empty()) {
        v.push(("if-modified-since", lm.to_string()));
    }
    v
}

/// What the stored answer under `key` is good for, for a request sending `request_headers` at `now`.
pub(crate) fn lookup(key: &str, request_headers: &[(String, String)], mode: Mode, now: f64) -> Use {
    if !mode.read {
        return Use::Miss;
    }
    let Some(e) = store().lock().ok().and_then(|mut m| m.get(key)) else { return Use::Miss };
    if mode.skip_validation {
        return Use::Fresh(e);
    }
    let vary_matches = e.vary.iter().all(|(name, stored)| header(request_headers, name) == stored.as_deref());
    let (freshness, _staleness) = lifetimes(e.status, &e.headers, e.response_time);
    if vary_matches && !mode.always_validate && freshness > current_age(&e, now) {
        return Use::Fresh(e);
    }
    let v = validators(&e);
    if v.is_empty() {
        return Use::Miss;
    }
    Use::Validate(v)
}

/// Whether a `GET`'s answer may be stored, and the [`Entry`] to store it as -- the storability rules shared by
/// the in-session [`put`] and the shared cache's [`shared_blob`]:
/// * `Storable::Remove`: drop any stored answer (`no-store` on either side);
/// * `Storable::No`:     leave any stored answer as it is (the request may not write, a `206`, a redirect, or
///   `Vary: *`);
/// * `Storable::Entry`:  store this (its `Set-Cookie` already removed, its `Vary` recorded from the request).
pub(crate) enum Storable {
    Remove,
    No,
    Entry(Entry),
}

/// Apply the storability rules ([`Storable`]) to a `GET`'s answer: what the request sent (for its `Vary`), what
/// came back, and when.
#[allow(clippy::too_many_arguments)]
pub(crate) fn storable(
    mode: Mode,
    request_headers: &[(String, String)],
    status: u16,
    version: wreq::Version,
    headers: &[(String, String)],
    body: &[u8],
    encoded_size: u64,
    request_time: f64,
    response_time: f64,
) -> Storable {
    let no_store = directives(headers).iter().any(|d| d == "no-store") || directives(request_headers).iter().any(|d| d == "no-store");
    let vary: Vec<String> = headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("vary"))
        .flat_map(|(_, v)| v.split(','))
        .map(|n| n.trim().to_ascii_lowercase())
        .filter(|n| !n.is_empty())
        .collect();
    if no_store {
        return Storable::Remove;
    }
    if !mode.write || status == 206 || (300..400).contains(&status) || vary.iter().any(|n| n == "*") {
        return Storable::No;
    }
    Storable::Entry(Entry {
        status,
        headers: headers.iter().filter(|(k, _)| !k.eq_ignore_ascii_case("set-cookie")).cloned().collect(),
        body: body.to_vec(),
        encoded_size,
        request_time,
        response_time,
        vary: vary.iter().map(|n| (n.clone(), header(request_headers, n).map(str::to_string))).collect(),
        version,
    })
}

/// Store a `GET`'s answer under `key`, when it may be stored: what the request sent (for its `Vary`), what came
/// back, and when.
#[allow(clippy::too_many_arguments)]
pub(crate) fn put(
    key: &str,
    mode: Mode,
    request_headers: &[(String, String)],
    status: u16,
    version: wreq::Version,
    headers: &[(String, String)],
    body: &[u8],
    encoded_size: u64,
    request_time: f64,
    response_time: f64,
) {
    let Ok(mut m) = store().lock() else { return };
    match storable(mode, request_headers, status, version, headers, body, encoded_size, request_time, response_time) {
        Storable::Remove => {
            m.remove(key);
        }
        Storable::No => {}
        Storable::Entry(e) => m.insert(key, e),
    }
}

/// Drop the stored answer under `key` (an unsafe method succeeded on its URL).
pub(crate) fn invalidate(key: &str) {
    if let Ok(mut m) = store().lock() {
        m.remove(key);
    }
}

// ── D10 (step 14, Frontier L): the SHARED response cache a plugin backs ──────────────────────────────
//
// The cache above is this process's own, in memory. D10 adds a second tier that a loaded plugin backs (Redis, a
// file, a cross-process map): korvus asks it for a stored answer before it fetches, and offers it an answer to
// store, over the `cache_get` / `cache_put` plugin slots (`plugin_host::cache_get` / `cache_put`, which trade the
// answer as base64). The plugin is only the store -- what an answer is keyed by, and how it is serialized, are
// korvus's, here:
//   * the KEY (`shared_key`) is the request's method, the same partition the in-session cache keys on, and the URL
//     without its fragment -- the browser cache key, folded into one string for a flat plugin store;
//   * `Vary` is the browser cache's secondary key: a stored blob records the request headers its answer varies on,
//     and `serve_match` serves it only when this request's values of those headers match (never the wrong variant);
//   * a blob holds the whole answer (status, headers sans `Set-Cookie`, body, the wire version, the recorded `Vary`,
//     the times), so another korvus process reconstructs the exact [`Entry`] and `deliver_stored` hands it on.
// Unlike the in-session cache, the plugin owns the blob's freshness and eviction (it is a shared store with its own
// lifetime), so a blob it returns is served as stored -- `serve_match` checks only `Vary`, not freshness. It is all
// reached only when a cache plugin is loaded (`plugin_host::has_cache`), so a run with none behaves exactly as it
// did before (invariant I9/J2).

/// The shared cache's wire version. A blob that does not open with this byte is read as a miss, so bumping it drops
/// every blob an older korvus wrote rather than misreading one.
const SHARED_WIRE: u8 = 1;

/// `wreq::Version` as one byte, for a blob (`SHARED_WIRE`). Anything but the five known versions rides as HTTP/1.1.
fn version_tag(v: wreq::Version) -> u8 {
    match v {
        wreq::Version::HTTP_09 => 9,
        wreq::Version::HTTP_10 => 10,
        wreq::Version::HTTP_2 => 20,
        wreq::Version::HTTP_3 => 30,
        _ => 11,
    }
}

/// The reverse of [`version_tag`]; an unknown tag reads back as HTTP/1.1.
fn version_of(tag: u8) -> wreq::Version {
    match tag {
        9 => wreq::Version::HTTP_09,
        10 => wreq::Version::HTTP_10,
        20 => wreq::Version::HTTP_2,
        30 => wreq::Version::HTTP_3,
        _ => wreq::Version::HTTP_11,
    }
}

/// Append `s` to a blob as a `u32` length (little-endian) then its bytes.
fn write_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// A bounds-checked little-endian reader over a blob; every read is `Option`, so a truncated or malformed blob
/// parses as `None` (a miss) and never panics.
struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Reader { b, i: 0 }
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.i.checked_add(n)?;
        let s = self.b.get(self.i..end)?;
        self.i = end;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|s| s[0])
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<usize> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?) as usize)
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn f64(&mut self) -> Option<f64> {
        Some(f64::from_bits(self.u64()?))
    }
    fn str(&mut self) -> Option<String> {
        let n = self.u32()?;
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }
}

/// Serialize a stored answer to the shared cache's self-describing byte blob, so another korvus process can
/// reconstruct the exact [`Entry`] with [`deserialize`].
pub(crate) fn serialize(e: &Entry) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(SHARED_WIRE);
    out.push(version_tag(e.version));
    out.extend_from_slice(&e.status.to_le_bytes());
    out.extend_from_slice(&e.encoded_size.to_le_bytes());
    out.extend_from_slice(&e.request_time.to_bits().to_le_bytes());
    out.extend_from_slice(&e.response_time.to_bits().to_le_bytes());
    out.extend_from_slice(&(e.headers.len() as u32).to_le_bytes());
    for (k, v) in &e.headers {
        write_str(&mut out, k);
        write_str(&mut out, v);
    }
    out.extend_from_slice(&(e.vary.len() as u32).to_le_bytes());
    for (name, val) in &e.vary {
        write_str(&mut out, name);
        match val {
            Some(v) => {
                out.push(1);
                write_str(&mut out, v);
            }
            None => out.push(0),
        }
    }
    out.extend_from_slice(&(e.body.len() as u32).to_le_bytes());
    out.extend_from_slice(&e.body);
    out
}

/// Reconstruct an [`Entry`] from a blob [`serialize`] wrote; `None` when it is truncated, malformed or a wire
/// version korvus does not read (all treated as a cache miss).
pub(crate) fn deserialize(bytes: &[u8]) -> Option<Entry> {
    let mut r = Reader::new(bytes);
    if r.u8()? != SHARED_WIRE {
        return None;
    }
    let version = version_of(r.u8()?);
    let status = r.u16()?;
    let encoded_size = r.u64()?;
    let request_time = r.f64()?;
    let response_time = r.f64()?;
    let nh = r.u32()?;
    let mut headers = Vec::with_capacity(nh.min(1024));
    for _ in 0..nh {
        let k = r.str()?;
        let v = r.str()?;
        headers.push((k, v));
    }
    let nv = r.u32()?;
    let mut vary = Vec::with_capacity(nv.min(64));
    for _ in 0..nv {
        let name = r.str()?;
        let val = match r.u8()? {
            0 => None,
            _ => Some(r.str()?),
        };
        vary.push((name, val));
    }
    let nb = r.u32()?;
    let body = r.take(nb)?.to_vec();
    Some(Entry { status, headers, body, encoded_size, request_time, response_time, vary, version })
}

/// The shared cache's key for a request: its `method`, the `partition` (the same network isolation key the
/// in-session cache uses) and the URL without its fragment, joined by a unit separator -- one flat string for a
/// plugin KV store that still carries the browser cache key's method + url + partition. `Vary` is a secondary key,
/// enforced from the stored blob in [`serve_match`].
pub(crate) fn shared_key(method: &str, partition: &str, url: &url::Url) -> String {
    let mut u = url.clone();
    u.set_fragment(None);
    format!("{method}\u{1f}{partition}\u{1f}{u}")
}

/// An [`Entry`] from a shared-cache blob, good to serve for a request sending `request_headers` only if it parses
/// and its recorded `Vary` headers all match -- a shared cache must never hand back the wrong variant. `None` on a
/// malformed blob or a `Vary` mismatch, so the request falls through to the in-session cache and the network.
pub(crate) fn serve_match(bytes: &[u8], request_headers: &[(String, String)]) -> Option<Entry> {
    let e = deserialize(bytes)?;
    let vary_matches = e.vary.iter().all(|(name, stored)| header(request_headers, name) == stored.as_deref());
    vary_matches.then_some(e)
}

/// The blob a cacheable `GET`'s answer would be stored as in the shared cache, or `None` when it may not be stored
/// (the storability rules of [`storable`]: `no-store` on either side, a `206`, a redirect, `Vary: *`, or a request
/// that may not write). korvus offers this to a cache plugin with `cache_put`; the plugin owns the store and its
/// lifetime. (A shared cache has no cross-process delete, so a `no-store`/unsafe-method invalidation drops only the
/// in-session entry here, and the plugin evicts on its own.)
#[allow(clippy::too_many_arguments)]
pub(crate) fn shared_blob(
    mode: Mode,
    request_headers: &[(String, String)],
    status: u16,
    version: wreq::Version,
    headers: &[(String, String)],
    body: &[u8],
    encoded_size: u64,
    request_time: f64,
    response_time: f64,
) -> Option<Vec<u8>> {
    match storable(mode, request_headers, status, version, headers, body, encoded_size, request_time, response_time) {
        Storable::Entry(e) => Some(serialize(&e)),
        Storable::Remove | Storable::No => None,
    }
}

/// A `304` for the stored answer under `key`: its headers update the stored ones (`HttpResponseHeaders::Update`:
/// every field the `304` carries replaces the stored one, but for the hop-by-hop, authentication and content ones,
/// which stay), the times are the revalidation's, and the updated answer is what is served.
pub(crate) fn refresh(key: &str, not_modified: &[(String, String)], request_time: f64, response_time: f64) -> Option<Entry> {
    const KEEP: &[&str] = &[
        "connection", "proxy-connection", "keep-alive", "www-authenticate", "proxy-authenticate", "proxy-authorization",
        "te", "trailer", "transfer-encoding", "upgrade", "content-location", "content-md5", "etag", "content-encoding",
        "content-range", "content-type", "content-length", "x-frame-options", "x-xss-protection",
    ];
    let kept = |n: &str| KEEP.iter().any(|k| n.eq_ignore_ascii_case(k)) || n.to_ascii_lowercase().starts_with("x-content-") || n.to_ascii_lowercase().starts_with("x-webkit-");
    let mut m = store().lock().ok()?;
    let mut e = m.get(key)?;
    let updated: Vec<&(String, String)> = not_modified.iter().filter(|(k, _)| !kept(k) && !k.eq_ignore_ascii_case("set-cookie")).collect();
    e.headers.retain(|(k, _)| !updated.iter().any(|(u, _)| u.eq_ignore_ascii_case(k)));
    e.headers.extend(updated.into_iter().cloned());
    e.request_time = request_time;
    e.response_time = response_time;
    // Stored again rather than edited in place, so the budget counts the updated headers.
    m.insert(key, e.clone());
    Some(e)
}

/// An HTTP date (RFC 9110 5.6.7): IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`), the obsolete RFC 850
/// (`Sunday, 06-Nov-94 08:49:37 GMT`) and asctime (`Sun Nov  6 08:49:37 1994`) forms, in seconds since the epoch.
pub(crate) fn parse_http_date(s: &str) -> Option<f64> {
    const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let month = |m: &str| MONTHS.iter().position(|x| m.eq_ignore_ascii_case(x)).map(|i| i as u32 + 1);
    let hms = |t: &str| -> Option<(u32, u32, u32)> {
        let mut it = t.split(':');
        let (h, m, sec): (u32, u32, u32) = (it.next()?.parse().ok()?, it.next()?.parse().ok()?, it.next()?.parse().ok()?);
        (it.next().is_none() && h < 24 && m < 60 && sec < 61).then_some((h, m, sec))
    };
    let s = s.trim();
    let (y, mo, d, t): (i64, u32, u32, (u32, u32, u32)) = match s.split_once(',') {
        Some((_, rest)) => {
            let parts: Vec<&str> = rest.split_whitespace().collect();
            match parts.as_slice() {
                // IMF-fixdate: `06 Nov 1994 08:49:37 GMT`
                [day, mon, year, time, gmt] if gmt.eq_ignore_ascii_case("gmt") => (year.parse().ok()?, month(mon)?, day.parse().ok()?, hms(time)?),
                // RFC 850: `06-Nov-94 08:49:37 GMT`
                [dmy, time, gmt] if gmt.eq_ignore_ascii_case("gmt") => {
                    let f: Vec<&str> = dmy.split('-').collect();
                    let [day, mon, year] = f.as_slice() else { return None };
                    let yy: i64 = year.parse().ok()?;
                    let year = if year.len() <= 2 { if yy < 70 { 2000 + yy } else { 1900 + yy } } else { yy };
                    (year, month(mon)?, day.parse().ok()?, hms(time)?)
                }
                _ => return None,
            }
        }
        None => {
            // asctime: `Sun Nov  6 08:49:37 1994`
            let parts: Vec<&str> = s.split_whitespace().collect();
            let [_, mon, day, time, year] = parts.as_slice() else { return None };
            (year.parse().ok()?, month(mon)?, day.parse().ok()?, hms(time)?)
        }
    };
    if !(1..=31).contains(&d) {
        return None;
    }
    let days = days_from_civil(y, mo, d);
    Some((days * 86_400 + (t.0 * 3600 + t.1 * 60 + t.2) as i64) as f64)
}

/// Days from 1970-01-01 to the civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn the_three_http_date_forms_parse() {
        for s in ["Sun, 06 Nov 1994 08:49:37 GMT", "Sunday, 06-Nov-94 08:49:37 GMT", "Sun Nov  6 08:49:37 1994"] {
            assert_eq!(parse_http_date(s), Some(784_111_777.0), "{s}");
        }
        assert_eq!(parse_http_date("Wed, 07 Oct 2026 12:10:03 GMT"), Some(1_791_375_003.0));
        assert_eq!(parse_http_date("0"), None);
        assert_eq!(parse_http_date("Sun, 32 Nov 1994 08:49:37 GMT"), None);
    }

    /// `GetFreshnessLifetimes`, rule by rule.
    #[test]
    fn freshness_is_chromes() {
        let t = 1_000_000.0;
        assert_eq!(lifetimes(200, &h(&[("cache-control", "max-age=300")]), t).0, 300.0);
        assert_eq!(lifetimes(200, &h(&[("cache-control", "public, no-cache, max-age=300")]), t).0, 0.0);
        assert_eq!(lifetimes(200, &h(&[("pragma", "no-cache"), ("cache-control", "max-age=60")]), t).0, 0.0);
        let dated = h(&[("date", "Sun, 06 Nov 1994 08:49:37 GMT"), ("expires", "Sun, 06 Nov 1994 09:49:37 GMT")]);
        assert_eq!(lifetimes(200, &dated, t).0, 3600.0);
        assert_eq!(lifetimes(200, &h(&[("date", "Sun, 06 Nov 1994 08:49:37 GMT"), ("expires", "0")]), t).0, 0.0);
        let heuristic = h(&[("date", "Sun, 16 Nov 1994 08:49:37 GMT"), ("last-modified", "Sun, 06 Nov 1994 08:49:37 GMT")]);
        assert_eq!(lifetimes(200, &heuristic, t).0, 86_400.0);
        assert_eq!(lifetimes(404, &heuristic, t).0, 0.0);
        assert_eq!(lifetimes(301, &h(&[]), t).0, f64::INFINITY);
    }

    /// Chrome 150 on player.fm: `max-age=300` with `age: 3178` is stale on arrival, so the next fetch of it goes out
    /// with `if-modified-since` from the stored `last-modified`; a `304` updates the stored headers and serves it.
    #[test]
    fn a_stale_on_arrival_answer_is_revalidated_with_its_last_modified() {
        let u = url::Url::parse("https://example.test/favicon.ico#x").unwrap();
        let k = key("https://example.test https://example.test", &u);
        assert!(k.ends_with(" https://example.test/favicon.ico"));
        let headers = h(&[
            ("date", "Thu, 08 Oct 2026 11:36:29 GMT"),
            ("cache-control", "max-age=300"),
            ("age", "3178"),
            ("last-modified", "Wed, 07 Oct 2026 12:10:03 GMT"),
            ("vary", "Origin"),
            ("set-cookie", "a=b"),
        ]);
        let now = parse_http_date("Thu, 08 Oct 2026 11:36:29 GMT").unwrap();
        let m = mode(None, &[]);
        put(&k, m, &[], 200, wreq::Version::HTTP_2, &headers, b"ICO", 3, now, now);
        match lookup(&k, &[], m, now + 0.148) {
            Use::Validate(v) => assert_eq!(v, vec![("if-modified-since", "Wed, 07 Oct 2026 12:10:03 GMT".to_string())]),
            _ => panic!("expected a validation"),
        }
        match lookup(&k, &[], mode(Some("force-cache"), &[]), now) {
            Use::Fresh(e) => assert!(!e.headers.iter().any(|(n, _)| n == "set-cookie"), "a stored answer keeps no Set-Cookie"),
            _ => panic!("expected the stored answer"),
        }
        let e = refresh(&k, &h(&[("date", "Thu, 08 Oct 2026 11:36:30 GMT"), ("age", "3179"), ("content-type", "text/html")]), now + 0.2, now + 0.3).unwrap();
        assert_eq!(header(&e.headers, "age"), Some("3179"));
        assert!(header(&e.headers, "content-type").is_none(), "a 304's content headers do not replace the stored ones");
        assert_eq!(e.body, b"ICO");
    }

    #[test]
    fn fresh_answers_vary_and_cache_modes() {
        let u = url::Url::parse("https://example.test/app.js").unwrap();
        let k = key("p-fresh", &u);
        let now = 2_000_000.0;
        let m = mode(None, &[]);
        put(&k, m, &h(&[("origin", "https://a.test")]), 200, wreq::Version::HTTP_2, &h(&[("cache-control", "max-age=60"), ("vary", "Origin"), ("etag", "\"v1\"")]), b"x", 1, now, now);
        assert!(matches!(lookup(&k, &h(&[("origin", "https://a.test")]), m, now + 10.0), Use::Fresh(_)));
        // another Origin: validated, by its ETag
        assert!(matches!(lookup(&k, &h(&[("origin", "https://b.test")]), m, now + 10.0), Use::Validate(ref v) if v[0].0 == "if-none-match"));
        // past max-age: validated
        assert!(matches!(lookup(&k, &h(&[("origin", "https://a.test")]), m, now + 61.0), Use::Validate(_)));
        // the modes
        assert!(matches!(lookup(&k, &h(&[("origin", "https://a.test")]), mode(Some("no-cache"), &[]), now + 10.0), Use::Validate(_)));
        assert!(matches!(lookup(&k, &[], mode(Some("force-cache"), &[]), now + 1e6), Use::Fresh(_)));
        assert!(matches!(lookup(&k, &[], mode(Some("reload"), &[]), now), Use::Miss));
        assert!(matches!(lookup(&k, &[], mode(None, &h(&[("If-None-Match", "\"v0\"")])), now), Use::Miss));
        // a no-store answer drops what was stored
        put(&k, m, &[], 200, wreq::Version::HTTP_2, &h(&[("cache-control", "no-store")]), b"y", 1, now, now);
        assert!(matches!(lookup(&k, &[], mode(Some("force-cache"), &[]), now), Use::Miss));
    }

    /// The cache is bounded as Chrome's memory backend is: least recently USED out first, a read counting as a use,
    /// and an answer over an eighth of the budget never stored.
    #[test]
    fn the_store_evicts_the_least_recently_used_and_refuses_oversized_answers() {
        let entry = |body: usize| Entry {
            status: 200,
            headers: Vec::new(),
            body: vec![0; body],
            encoded_size: body as u64,
            request_time: 0.0,
            response_time: 0.0,
            vary: Vec::new(),
            version: wreq::Version::HTTP_2,
        };
        let mut s = Store::new(800);
        for k in ["a", "b", "c", "d"] {
            s.insert(k, entry(99)); // 100 bytes with its one-byte key
        }
        assert!(s.get("a").is_some(), "a read makes `a` the most recently used");
        for k in ["e", "f", "g", "h", "i"] {
            s.insert(k, entry(99));
        }
        assert_eq!(s.bytes, 800, "the budget holds exactly");
        assert!(s.get("b").is_none(), "the least recently used went first");
        assert!(s.get("a").is_some(), "`a` was read, so it outlived `b`");
        s.insert("big", entry(101 - 3));
        assert!(s.get("big").is_none(), "an answer over an eighth of the budget (101 > 100 bytes) is not stored");
        let before = s.bytes;
        s.insert("a", entry(49));
        assert_eq!(s.bytes, before - 50, "replacing an answer counts the new one, not both");
        s.remove("a");
        assert!(s.get("a").is_none());
        assert_eq!(s.bytes, before - 100);
    }

    /// D10: a shared-cache blob round-trips byte-exact, its `Vary` gates which request it serves, and the key
    /// carries method + partition + url without the fragment.
    #[test]
    fn a_shared_blob_round_trips_and_vary_gates_it() {
        let now = 3_000_000.0;
        let m = mode(None, &[]);
        let req = h(&[("accept-encoding", "gzip")]);
        let resp = h(&[("content-type", "text/html"), ("vary", "Accept-Encoding"), ("set-cookie", "a=b")]);
        let blob = shared_blob(m, &req, 200, wreq::Version::HTTP_2, &resp, b"<html>", 6, now, now).expect("storable");
        // the same varying request header: a hit, with Set-Cookie dropped and the bytes/version exact
        let e = serve_match(&blob, &req).expect("vary match");
        assert_eq!(e.status, 200);
        assert_eq!(e.body, b"<html>");
        assert_eq!(e.version, wreq::Version::HTTP_2);
        assert!(!e.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("set-cookie")));
        // a different Accept-Encoding is the wrong variant -- not served
        assert!(serve_match(&blob, &h(&[("accept-encoding", "br")])).is_none());
        // no-store is never offered to the shared cache, and a truncated blob is a miss, not a panic
        assert!(shared_blob(m, &[], 200, wreq::Version::HTTP_2, &h(&[("cache-control", "no-store")]), b"x", 1, now, now).is_none());
        assert!(serve_match(&blob[..blob.len() / 2], &req).is_none());
        let u = url::Url::parse("https://e.test/a?q=1#frag").unwrap();
        assert_eq!(shared_key("GET", "p", &u), "GET\u{1f}p\u{1f}https://e.test/a?q=1");
    }
}
