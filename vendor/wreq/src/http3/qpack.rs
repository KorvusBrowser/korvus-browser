//! KORVUS PATCH (H3): QPACK (RFC 9204) as Chrome 150's QUICHE runs it.
//!
//! The ENCODER is a port of `QpackEncoder` (`quic/core/qpack/qpack_encoder.cc`): its first pass decides, field by
//! field, between the static table, the dynamic table (inserting through the encoder stream whenever the peer's
//! table and blocked-stream budget allow), a name reference and a literal -- with the same draining fraction (0.25),
//! the same eviction guard (`MaxInsertSizeWithoutEvictingGivenEntry`), the same blocking accounting
//! (`NewQpackBlockingManager`) -- and its second pass writes every reference relative to a Base equal to the Required
//! Insert Count. Strings are Huffman-coded exactly when that is shorter (`QpackInstructionEncoder::DoStartString`);
//! `cookie` is split at `;` (and an optional space) and any other value at NUL (`ValueSplittingHeaderList`).
//!
//! The DECODER follows `QpackDecoder`: inserts from the peer's encoder stream, blocked field sections held until
//! their Required Insert Count arrives, and after each decoded section a Section Acknowledgement (when it referred
//! to the table) then an Insert Count Increment for whatever the acknowledgement did not cover
//! (`QpackDecoder::OnDecodingCompleted`).

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::OnceLock,
};

use super::huffman;

/// The QPACK static table (RFC 9204 Appendix A), as `qpack_static_table.cc` lists it.
pub(crate) const STATIC_TABLE: [(&str, &str); 99] = [
    (":authority", ""),
    (":path", "/"),
    ("age", "0"),
    ("content-disposition", ""),
    ("content-length", "0"),
    ("cookie", ""),
    ("date", ""),
    ("etag", ""),
    ("if-modified-since", ""),
    ("if-none-match", ""),
    ("last-modified", ""),
    ("link", ""),
    ("location", ""),
    ("referer", ""),
    ("set-cookie", ""),
    (":method", "CONNECT"),
    (":method", "DELETE"),
    (":method", "GET"),
    (":method", "HEAD"),
    (":method", "OPTIONS"),
    (":method", "POST"),
    (":method", "PUT"),
    (":scheme", "http"),
    (":scheme", "https"),
    (":status", "103"),
    (":status", "200"),
    (":status", "304"),
    (":status", "404"),
    (":status", "503"),
    ("accept", "*/*"),
    ("accept", "application/dns-message"),
    ("accept-encoding", "gzip, deflate, br"),
    ("accept-ranges", "bytes"),
    ("access-control-allow-headers", "cache-control"),
    ("access-control-allow-headers", "content-type"),
    ("access-control-allow-origin", "*"),
    ("cache-control", "max-age=0"),
    ("cache-control", "max-age=2592000"),
    ("cache-control", "max-age=604800"),
    ("cache-control", "no-cache"),
    ("cache-control", "no-store"),
    ("cache-control", "public, max-age=31536000"),
    ("content-encoding", "br"),
    ("content-encoding", "gzip"),
    ("content-type", "application/dns-message"),
    ("content-type", "application/javascript"),
    ("content-type", "application/json"),
    ("content-type", "application/x-www-form-urlencoded"),
    ("content-type", "image/gif"),
    ("content-type", "image/jpeg"),
    ("content-type", "image/png"),
    ("content-type", "text/css"),
    ("content-type", "text/html; charset=utf-8"),
    ("content-type", "text/plain"),
    ("content-type", "text/plain;charset=utf-8"),
    ("range", "bytes=0-"),
    ("strict-transport-security", "max-age=31536000"),
    ("strict-transport-security", "max-age=31536000; includesubdomains"),
    ("strict-transport-security", "max-age=31536000; includesubdomains; preload"),
    ("vary", "accept-encoding"),
    ("vary", "origin"),
    ("x-content-type-options", "nosniff"),
    ("x-xss-protection", "1; mode=block"),
    (":status", "100"),
    (":status", "204"),
    (":status", "206"),
    (":status", "302"),
    (":status", "400"),
    (":status", "403"),
    (":status", "421"),
    (":status", "425"),
    (":status", "500"),
    ("accept-language", ""),
    ("access-control-allow-credentials", "FALSE"),
    ("access-control-allow-credentials", "TRUE"),
    ("access-control-allow-headers", "*"),
    ("access-control-allow-methods", "get"),
    ("access-control-allow-methods", "get, post, options"),
    ("access-control-allow-methods", "options"),
    ("access-control-expose-headers", "content-length"),
    ("access-control-request-headers", "content-type"),
    ("access-control-request-method", "get"),
    ("access-control-request-method", "post"),
    ("alt-svc", "clear"),
    ("authorization", ""),
    ("content-security-policy", "script-src 'none'; object-src 'none'; base-uri 'none'"),
    ("early-data", "1"),
    ("expect-ct", ""),
    ("forwarded", ""),
    ("if-range", ""),
    ("origin", ""),
    ("purpose", "prefetch"),
    ("server", ""),
    ("timing-allow-origin", "*"),
    ("upgrade-insecure-requests", "1"),
    ("user-agent", ""),
    ("x-forwarded-for", ""),
    ("x-frame-options", "deny"),
    ("x-frame-options", "sameorigin"),
];

struct StaticIndex {
    full: HashMap<(&'static str, &'static str), u64>,
    name: HashMap<&'static str, u64>,
}

/// The first entry of each (name, value) and of each name, as `HpackStaticTable::Initialize` keeps them (an insert
/// that collides keeps the earlier index).
fn static_index() -> &'static StaticIndex {
    static IDX: OnceLock<StaticIndex> = OnceLock::new();
    IDX.get_or_init(|| {
        let mut full = HashMap::new();
        let mut name = HashMap::new();
        for (i, (n, v)) in STATIC_TABLE.iter().enumerate() {
            full.entry((*n, *v)).or_insert(i as u64);
            name.entry(*n).or_insert(i as u64);
        }
        StaticIndex { full, name }
    })
}

/// An entry's size (RFC 9204 3.2.1).
fn entry_size(name: &[u8], value: &[u8]) -> u64 {
    (name.len() + value.len() + 32) as u64
}

// ===== integers and strings (RFC 7541 5.1, 5.2) =====

/// Writes `v` with an `n`-bit prefix into the low bits of `first` (whose high bits carry the instruction's flags).
fn put_int(out: &mut Vec<u8>, first: u8, n: u8, v: u64) {
    let max = (1u64 << n) - 1;
    if v < max {
        out.push(first | v as u8);
        return;
    }
    out.push(first | max as u8);
    let mut rest = v - max;
    while rest >= 128 {
        out.push((rest % 128) as u8 | 0x80);
        rest /= 128;
    }
    out.push(rest as u8);
}

fn get_int(b: &[u8], at: &mut usize, n: u8) -> Option<u64> {
    let max = (1u64 << n) - 1;
    let mut v = u64::from(*b.get(*at)?) & max;
    *at += 1;
    if v < max {
        return Some(v);
    }
    let mut shift = 0u32;
    loop {
        let byte = *b.get(*at)?;
        *at += 1;
        v = v.checked_add(u64::from(byte & 0x7f).checked_shl(shift)?)?;
        if byte & 0x80 == 0 {
            return Some(v);
        }
        shift += 7;
        if shift > 62 {
            return None;
        }
    }
}

/// A string with its H bit at `1 << n` of `first` and an `n`-bit length prefix: Huffman when that is shorter.
fn put_string(out: &mut Vec<u8>, first: u8, n: u8, s: &[u8], huffman: bool) {
    let hlen = huffman::encoded_len(s);
    if huffman && hlen < s.len() {
        put_int(out, first | (1 << n), n, hlen as u64);
        huffman::encode(s, out);
    } else {
        put_int(out, first, n, s.len() as u64);
        out.extend_from_slice(s);
    }
}

fn get_string(b: &[u8], at: &mut usize, n: u8) -> Option<Vec<u8>> {
    let h = *b.get(*at)? & (1 << n) != 0;
    let len = get_int(b, at, n)? as usize;
    let raw = b.get(*at..*at + len)?;
    *at += len;
    if h { huffman::decode(raw) } else { Some(raw.to_vec()) }
}

// ===== the value-splitting header list =====

/// The fields a header list is encoded as: `cookie` crumbled at `;` (eating one optional space after it), any
/// other value split at NUL (`value_splitting_header_list.cc`).
fn split_fields(headers: &[(Vec<u8>, Vec<u8>)], cookie_crumbling: bool) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    for (name, value) in headers {
        let is_cookie = name.as_slice() == b"cookie";
        let sep = if is_cookie { b';' } else { 0 };
        if is_cookie && !cookie_crumbling {
            out.push((name.clone(), value.clone()));
            continue;
        }
        let mut start = 0;
        loop {
            match value[start..].iter().position(|c| *c == sep) {
                Some(rel) => {
                    let end = start + rel;
                    out.push((name.clone(), value[start..end].to_vec()));
                    start = end + 1;
                    if is_cookie && value.get(start) == Some(&b' ') && start + 1 <= value.len() && end + 1 < value.len() {
                        start += 1;
                    }
                }
                None => {
                    out.push((name.clone(), value[start..].to_vec()));
                    break;
                }
            }
        }
    }
    out
}

// ===== the encoder's dynamic table =====

struct EncoderTable {
    entries: VecDeque<(Vec<u8>, Vec<u8>)>,
    dropped: u64,
    size: u64,
    capacity: u64,
    max_capacity: u64,
    max_entries: u64,
    full_index: HashMap<(Vec<u8>, Vec<u8>), u64>,
    name_index: HashMap<Vec<u8>, u64>,
}

enum Match {
    Full { is_static: bool, index: u64 },
    Name { is_static: bool, index: u64 },
    None,
}

impl EncoderTable {
    fn new() -> Self {
        EncoderTable {
            entries: VecDeque::new(),
            dropped: 0,
            size: 0,
            capacity: 0,
            max_capacity: 0,
            max_entries: 0,
            full_index: HashMap::new(),
            name_index: HashMap::new(),
        }
    }

    fn inserted(&self) -> u64 {
        self.dropped + self.entries.len() as u64
    }

    fn remove_oldest(&mut self) {
        let Some((name, value)) = self.entries.pop_front() else { return };
        let index = self.dropped;
        let key = (name, value);
        if self.full_index.get(&key) == Some(&index) {
            self.full_index.remove(&key);
        }
        if self.name_index.get(&key.0) == Some(&index) {
            self.name_index.remove(&key.0);
        }
        self.size -= entry_size(&key.0, &key.1);
        self.dropped += 1;
    }

    fn evict_down_to(&mut self, capacity: u64) {
        while self.size > capacity && !self.entries.is_empty() {
            self.remove_oldest();
        }
    }

    fn insert(&mut self, name: &[u8], value: &[u8]) -> u64 {
        let index = self.inserted();
        let size = entry_size(name, value);
        self.evict_down_to(self.capacity.saturating_sub(size));
        self.size += size;
        self.entries.push_back((name.to_vec(), value.to_vec()));
        self.full_index.insert((name.to_vec(), value.to_vec()), index);
        self.name_index.insert(name.to_vec(), index);
        index
    }

    fn find(&self, name: &[u8], value: &[u8]) -> Match {
        let s = static_index();
        if let (Ok(n), Ok(v)) = (std::str::from_utf8(name), std::str::from_utf8(value)) {
            if let Some(i) = s.full.get(&(n, v)) {
                return Match::Full { is_static: true, index: *i };
            }
        }
        if let Some(i) = self.full_index.get(&(name.to_vec(), value.to_vec())) {
            return Match::Full { is_static: false, index: *i };
        }
        self.find_name(name)
    }

    fn find_name(&self, name: &[u8]) -> Match {
        if let Ok(n) = std::str::from_utf8(name) {
            if let Some(i) = static_index().name.get(n) {
                return Match::Name { is_static: true, index: *i };
            }
        }
        match self.name_index.get(name) {
            Some(i) => Match::Name { is_static: false, index: *i },
            None => Match::None,
        }
    }

    fn max_insert_size_without_evicting(&self, index: u64) -> u64 {
        if index > self.inserted() {
            return self.capacity;
        }
        let mut max = self.capacity - self.size;
        let mut entry_index = self.dropped;
        for (n, v) in &self.entries {
            if entry_index >= index {
                break;
            }
            entry_index += 1;
            max += entry_size(n, v);
        }
        max
    }

    fn draining_index(&self, fraction: f64) -> u64 {
        let required = (fraction * self.capacity as f64) as u64;
        let mut space = self.capacity - self.size;
        if self.entries.is_empty() || space >= required {
            return self.dropped;
        }
        let mut entry_index = self.dropped;
        let mut it = self.entries.iter();
        while space < required {
            match it.next() {
                Some((n, v)) => space += entry_size(n, v),
                None => return self.inserted(),
            }
            entry_index += 1;
            if entry_index == self.inserted() {
                return self.inserted();
            }
        }
        entry_index
    }
}

// ===== NewQpackBlockingManager =====

#[derive(Clone, Copy)]
struct IndexSet {
    min: u64,
    max: u64,
}

impl IndexSet {
    fn required_insert_count(&self) -> u64 {
        self.max + 1
    }
}

#[derive(Default)]
struct Blocking {
    known_received_count: u64,
    streams: HashMap<u64, VecDeque<IndexSet>>,
    blocked: Vec<u64>,
    min_index_refs: BTreeMap<u64, u64>,
}

impl Blocking {
    fn max_required(&self, stream: u64) -> u64 {
        self.streams.get(&stream).map(|b| b.iter().map(IndexSet::required_insert_count).max().unwrap_or(0)).unwrap_or(0)
    }

    fn update_blocked(&mut self, stream: u64) {
        let blocked = self.max_required(stream) > self.known_received_count;
        let listed = self.blocked.contains(&stream);
        if blocked && !listed {
            self.blocked.push(stream);
        } else if !blocked && listed {
            self.blocked.retain(|s| *s != stream);
        }
    }

    fn increase_known(&mut self, count: u64) {
        if count <= self.known_received_count {
            return;
        }
        self.known_received_count = count;
        let known = self.known_received_count;
        let streams = &self.streams;
        self.blocked.retain(|s| streams.get(s).map(|b| b.iter().map(IndexSet::required_insert_count).max().unwrap_or(0)).unwrap_or(0) > known);
    }

    fn dec_min(&mut self, min: u64) {
        if let Some(n) = self.min_index_refs.get_mut(&min) {
            *n -= 1;
            if *n == 0 {
                self.min_index_refs.remove(&min);
            }
        }
    }

    fn on_header_ack(&mut self, stream: u64) -> bool {
        let Some(blocks) = self.streams.get_mut(&stream) else { return false };
        let Some(first) = blocks.pop_front() else { return false };
        let empty = blocks.is_empty();
        if self.known_received_count < first.required_insert_count() {
            self.increase_known(first.required_insert_count());
        }
        self.dec_min(first.min);
        if empty {
            self.streams.remove(&stream);
            self.blocked.retain(|s| *s != stream);
        }
        true
    }

    fn on_stream_cancellation(&mut self, stream: u64) {
        if let Some(blocks) = self.streams.remove(&stream) {
            for b in blocks {
                self.dec_min(b.min);
            }
        }
        self.blocked.retain(|s| *s != stream);
    }

    fn on_block_sent(&mut self, stream: u64, set: IndexSet) {
        *self.min_index_refs.entry(set.min).or_insert(0) += 1;
        self.streams.entry(stream).or_default().push_back(set);
        self.update_blocked(stream);
    }

    fn blocking_allowed(&self, stream: u64, max_blocked: u64) -> bool {
        (self.blocked.len() as u64) < max_blocked || self.blocked.contains(&stream)
    }

    fn smallest_blocking_index(&self) -> u64 {
        self.min_index_refs.keys().next().copied().unwrap_or(u64::MAX)
    }
}

// ===== the encoder =====

enum Repr {
    Indexed { is_static: bool, index: u64 },
    NameRef { is_static: bool, index: u64, value: Vec<u8> },
    Literal { name: Vec<u8>, value: Vec<u8> },
}

/// QPACK encoder state of one connection.
pub(crate) struct Encoder {
    table: EncoderTable,
    blocking: Blocking,
    max_blocked_streams: u64,
    /// Encoder-stream instructions not yet handed to the stream.
    pending: Vec<u8>,
    huffman: bool,
    cookie_crumbling: bool,
}

const DRAINING_FRACTION: f64 = 0.25;

fn ref_set(referred: &mut Option<IndexSet>, index: u64) {
    match referred {
        Some(s) => {
            s.min = s.min.min(index);
            s.max = s.max.max(index);
        }
        None => *referred = Some(IndexSet { min: index, max: index }),
    }
}

impl Encoder {
    pub(crate) fn new(huffman: bool, cookie_crumbling: bool) -> Self {
        Encoder {
            table: EncoderTable::new(),
            blocking: Blocking::default(),
            max_blocked_streams: 0,
            pending: Vec::new(),
            huffman,
            cookie_crumbling,
        }
    }

    /// The peer's SETTINGS_QPACK_MAX_TABLE_CAPACITY: the table this encoder uses is `min(peer, own_limit)`, announced
    /// on the encoder stream with the next field section (`QuicSpdySession::OnSetting`, `:1219-1243`; the
    /// instruction waits in the sender's buffer until `FirstPassEncode` flushes it).
    pub(crate) fn set_peer_max_table_capacity(&mut self, max_table_capacity: u64, own_limit: u64) {
        if self.table.max_capacity == 0 {
            self.table.max_capacity = max_table_capacity;
            self.table.max_entries = max_table_capacity / 32;
        }
        let cap = max_table_capacity.min(own_limit);
        put_int(&mut self.pending, 0x20, 5, cap);
        self.table.capacity = cap;
        self.table.evict_down_to(cap);
    }

    /// The peer's SETTINGS_QPACK_BLOCKED_STREAMS (it may only grow, `SetMaximumBlockedStreams`).
    pub(crate) fn set_peer_blocked_streams(&mut self, blocked_streams: u64) {
        if blocked_streams >= self.max_blocked_streams {
            self.max_blocked_streams = blocked_streams;
        }
    }

    /// Encoder-stream bytes produced since the last call.
    pub(crate) fn take_encoder_stream(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }

    /// The peer's decoder-stream instructions.
    pub(crate) fn on_decoder_stream(&mut self, buf: &mut Vec<u8>) -> Result<(), ()> {
        let mut at = 0;
        loop {
            let start = at;
            let Some(&b) = buf.get(at) else { break };
            let r = if b & 0x80 != 0 {
                get_int(buf, &mut at, 7).map(|s| {
                    self.blocking.on_header_ack(s);
                })
            } else if b & 0x40 != 0 {
                get_int(buf, &mut at, 6).map(|s| self.blocking.on_stream_cancellation(s))
            } else {
                get_int(buf, &mut at, 6).map(|inc| {
                    let k = self.blocking.known_received_count.saturating_add(inc);
                    self.blocking.increase_known(k);
                })
            };
            if r.is_none() {
                at = start;
                break;
            }
        }
        buf.drain(..at);
        Ok(())
    }

    /// Encode one header list for request stream `stream`; the encoder-stream instructions it needs are left in
    /// [`Encoder::take_encoder_stream`].
    pub(crate) fn encode(&mut self, stream: u64, headers: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
        let known = self.blocking.known_received_count;
        let mut smallest_non_evictable = self.blocking.smallest_blocking_index().min(known);
        let draining = self.table.draining_index(DRAINING_FRACTION);
        let blocking_allowed = self.blocking.blocking_allowed(stream, self.max_blocked_streams);
        let mut referred: Option<IndexSet> = None;
        let mut reprs = Vec::new();

        for (name, value) in split_fields(headers, self.cookie_crumbling) {
            let size = entry_size(&name, &value);
            let mut m = self.table.find(&name, &value);
            if let Match::Full { is_static, index } = m {
                if is_static {
                    reprs.push(Repr::Indexed { is_static: true, index });
                    continue;
                }
                if index >= draining {
                    if blocking_allowed || index < known {
                        reprs.push(Repr::Indexed { is_static: false, index });
                        ref_set(&mut referred, index);
                        smallest_non_evictable = smallest_non_evictable.min(index);
                        continue;
                    }
                } else if blocking_allowed
                    && size <= self.table.max_insert_size_without_evicting(smallest_non_evictable.min(index))
                {
                    // Duplicate the draining entry and refer to the copy.
                    let relative = self.table.inserted() - 1 - index;
                    put_int(&mut self.pending, 0x00, 5, relative);
                    let new_index = self.table.insert(&name, &value);
                    reprs.push(Repr::Indexed { is_static: false, index: new_index });
                    ref_set(&mut referred, new_index);
                    smallest_non_evictable = smallest_non_evictable.min(index);
                    continue;
                }
                // The full match cannot be used: fall back to its name, unless that is the same entry.
                let name_match = self.table.find_name(&name);
                match name_match {
                    Match::Name { is_static: s2, index: i2 } if !(s2 == is_static && i2 == index) => {
                        m = Match::Name { is_static: s2, index: i2 }
                    }
                    _ => {
                        reprs.push(Repr::Literal { name, value });
                        continue;
                    }
                }
            }
            match m {
                Match::Name { is_static: true, index } => {
                    if blocking_allowed && size <= self.table.max_insert_size_without_evicting(smallest_non_evictable) {
                        // Insert with a static name reference and refer to the new entry.
                        put_int(&mut self.pending, 0xc0, 6, index);
                        put_string(&mut self.pending, 0x00, 7, &value, self.huffman);
                        let new_index = self.table.insert(&name, &value);
                        reprs.push(Repr::Indexed { is_static: false, index: new_index });
                        ref_set(&mut referred, new_index);
                        smallest_non_evictable = smallest_non_evictable.min(new_index);
                    } else {
                        reprs.push(Repr::NameRef { is_static: true, index, value });
                    }
                }
                Match::Name { is_static: false, index } => {
                    if blocking_allowed && size <= self.table.max_insert_size_without_evicting(smallest_non_evictable.min(index)) {
                        let relative = self.table.inserted() - 1 - index;
                        put_int(&mut self.pending, 0x80, 6, relative);
                        put_string(&mut self.pending, 0x00, 7, &value, self.huffman);
                        let new_index = self.table.insert(&name, &value);
                        reprs.push(Repr::Indexed { is_static: false, index: new_index });
                        ref_set(&mut referred, new_index);
                        smallest_non_evictable = smallest_non_evictable.min(index);
                    } else if (blocking_allowed || index < known) && index >= draining {
                        reprs.push(Repr::NameRef { is_static: false, index, value });
                        ref_set(&mut referred, index);
                        smallest_non_evictable = smallest_non_evictable.min(index);
                    } else {
                        reprs.push(Repr::Literal { name, value });
                    }
                }
                Match::None => {
                    if blocking_allowed && size <= self.table.max_insert_size_without_evicting(smallest_non_evictable) {
                        // Insert with a literal name and refer to the new entry.
                        put_string(&mut self.pending, 0x40, 5, &name, self.huffman);
                        put_string(&mut self.pending, 0x00, 7, &value, self.huffman);
                        let new_index = self.table.insert(&name, &value);
                        reprs.push(Repr::Indexed { is_static: false, index: new_index });
                        ref_set(&mut referred, new_index);
                        smallest_non_evictable = smallest_non_evictable.min(new_index);
                    } else {
                        reprs.push(Repr::Literal { name, value });
                    }
                }
                Match::Full { .. } => unreachable!("a full match is resolved above"),
            }
        }

        let ric = referred.map(|s| s.required_insert_count()).unwrap_or(0);
        if let Some(set) = referred {
            self.blocking.on_block_sent(stream, set);
        }

        // Second pass: the prefix, then every representation relative to Base = Required Insert Count.
        let mut out = Vec::new();
        let encoded_ric = if ric == 0 { 0 } else { ric % (2 * self.table.max_entries.max(1)) + 1 };
        put_int(&mut out, 0x00, 8, encoded_ric);
        put_int(&mut out, 0x00, 7, 0);
        let base = ric;
        for r in reprs {
            match r {
                Repr::Indexed { is_static: true, index } => put_int(&mut out, 0xc0, 6, index),
                Repr::Indexed { is_static: false, index } => put_int(&mut out, 0x80, 6, base - 1 - index),
                Repr::NameRef { is_static, index, value } => {
                    let (flag, idx) = if is_static { (0x50, index) } else { (0x40, base - 1 - index) };
                    put_int(&mut out, flag, 4, idx);
                    put_string(&mut out, 0x00, 7, &value, self.huffman);
                }
                Repr::Literal { name, value } => {
                    put_string(&mut out, 0x20, 3, &name, self.huffman);
                    put_string(&mut out, 0x00, 7, &value, self.huffman);
                }
            }
        }
        out
    }
}

// ===== the decoder =====

/// What decoding a field section came to.
pub(crate) enum Decoded {
    Headers(Vec<(Vec<u8>, Vec<u8>)>),
    /// Its Required Insert Count is ahead of the table; retry once more of the encoder stream has arrived.
    Blocked,
    Error,
}

/// QPACK decoder state of one connection.
pub(crate) struct Decoder {
    entries: VecDeque<(Vec<u8>, Vec<u8>)>,
    dropped: u64,
    size: u64,
    capacity: u64,
    max_capacity: u64,
    known_received_count: u64,
    /// Decoder-stream instructions not yet handed to the stream.
    pending: Vec<u8>,
}

impl Decoder {
    /// A decoder whose table may grow to `max_capacity` (this side's SETTINGS_QPACK_MAX_TABLE_CAPACITY).
    pub(crate) fn new(max_capacity: u64) -> Self {
        Decoder {
            entries: VecDeque::new(),
            dropped: 0,
            size: 0,
            capacity: 0,
            max_capacity,
            known_received_count: 0,
            pending: Vec::new(),
        }
    }

    fn inserted(&self) -> u64 {
        self.dropped + self.entries.len() as u64
    }

    fn insert(&mut self, name: Vec<u8>, value: Vec<u8>) -> bool {
        let size = entry_size(&name, &value);
        if size > self.capacity {
            return false;
        }
        while self.size + size > self.capacity {
            if let Some((n, v)) = self.entries.pop_front() {
                self.size -= entry_size(&n, &v);
                self.dropped += 1;
            }
        }
        self.size += size;
        self.entries.push_back((name, value));
        true
    }

    fn get(&self, abs: u64) -> Option<&(Vec<u8>, Vec<u8>)> {
        abs.checked_sub(self.dropped).and_then(|i| self.entries.get(i as usize))
    }

    /// The peer's encoder-stream instructions; `Err` is a connection error (QPACK_ENCODER_STREAM_ERROR).
    pub(crate) fn on_encoder_stream(&mut self, buf: &mut Vec<u8>) -> Result<(), ()> {
        let mut at = 0;
        loop {
            let start = at;
            let Some(&b) = buf.get(at) else { break };
            let done = if b & 0x80 != 0 {
                // Insert with Name Reference.
                let is_static = b & 0x40 != 0;
                let Some(idx) = get_int(buf, &mut at, 6) else { at = start; break };
                let Some(value) = get_string(buf, &mut at, 7) else { at = start; break };
                let name = if is_static {
                    STATIC_TABLE.get(idx as usize).map(|(n, _)| n.as_bytes().to_vec())
                } else {
                    self.inserted().checked_sub(1 + idx).and_then(|a| self.get(a)).map(|(n, _)| n.clone())
                };
                match name {
                    Some(n) => self.insert(n, value),
                    None => false,
                }
            } else if b & 0x40 != 0 {
                // Insert with Literal Name.
                let Some(name) = get_string(buf, &mut at, 5) else { at = start; break };
                let Some(value) = get_string(buf, &mut at, 7) else { at = start; break };
                self.insert(name, value)
            } else if b & 0x20 != 0 {
                // Set Dynamic Table Capacity.
                let Some(cap) = get_int(buf, &mut at, 5) else { at = start; break };
                if cap > self.max_capacity {
                    false
                } else {
                    self.capacity = cap;
                    while self.size > self.capacity {
                        if let Some((n, v)) = self.entries.pop_front() {
                            self.size -= entry_size(&n, &v);
                            self.dropped += 1;
                        }
                    }
                    true
                }
            } else {
                // Duplicate.
                let Some(idx) = get_int(buf, &mut at, 5) else { at = start; break };
                match self.inserted().checked_sub(1 + idx).and_then(|a| self.get(a)).cloned() {
                    Some((n, v)) => self.insert(n, v),
                    None => false,
                }
            };
            if !done {
                return Err(());
            }
        }
        buf.drain(..at);
        Ok(())
    }

    /// Decode the field section of a HEADERS frame on `stream`.
    pub(crate) fn decode(&mut self, stream: u64, block: &[u8]) -> Decoded {
        let mut at = 0;
        let Some(encoded_ric) = get_int(block, &mut at, 8) else { return Decoded::Error };
        let max_entries = self.max_capacity / 32;
        let ric = if encoded_ric == 0 {
            0
        } else {
            // RFC 9204 4.5.1.1.
            let full_range = 2 * max_entries;
            if max_entries == 0 || encoded_ric > full_range {
                return Decoded::Error;
            }
            let max_value = self.inserted() + max_entries;
            let max_wrapped = (max_value / full_range) * full_range;
            let mut ric = max_wrapped + encoded_ric - 1;
            if ric > max_value {
                if ric <= full_range {
                    return Decoded::Error;
                }
                ric -= full_range;
            }
            if ric == 0 {
                return Decoded::Error;
            }
            ric
        };
        if ric > self.inserted() {
            return Decoded::Blocked;
        }
        let Some(&sign_byte) = block.get(at) else { return Decoded::Error };
        let negative = sign_byte & 0x80 != 0;
        let Some(delta) = get_int(block, &mut at, 7) else { return Decoded::Error };
        let base = if negative { ric.checked_sub(delta + 1) } else { ric.checked_add(delta) };
        let Some(base) = base else { return Decoded::Error };

        let mut out = Vec::new();
        while at < block.len() {
            let b = block[at];
            let field = if b & 0x80 != 0 {
                let is_static = b & 0x40 != 0;
                let Some(idx) = get_int(block, &mut at, 6) else { return Decoded::Error };
                if is_static {
                    STATIC_TABLE.get(idx as usize).map(|(n, v)| (n.as_bytes().to_vec(), v.as_bytes().to_vec()))
                } else {
                    base.checked_sub(1 + idx).and_then(|a| self.get(a)).cloned()
                }
            } else if b & 0x40 != 0 {
                let is_static = b & 0x10 != 0;
                let Some(idx) = get_int(block, &mut at, 4) else { return Decoded::Error };
                let Some(value) = get_string(block, &mut at, 7) else { return Decoded::Error };
                let name = if is_static {
                    STATIC_TABLE.get(idx as usize).map(|(n, _)| n.as_bytes().to_vec())
                } else {
                    base.checked_sub(1 + idx).and_then(|a| self.get(a)).map(|(n, _)| n.clone())
                };
                name.map(|n| (n, value))
            } else if b & 0x20 != 0 {
                let Some(name) = get_string(block, &mut at, 3) else { return Decoded::Error };
                let Some(value) = get_string(block, &mut at, 7) else { return Decoded::Error };
                Some((name, value))
            } else if b & 0x10 != 0 {
                let Some(idx) = get_int(block, &mut at, 4) else { return Decoded::Error };
                self.get(base + idx).cloned()
            } else {
                let Some(idx) = get_int(block, &mut at, 3) else { return Decoded::Error };
                let Some(value) = get_string(block, &mut at, 7) else { return Decoded::Error };
                self.get(base + idx).map(|(n, _)| (n.clone(), value))
            };
            match field {
                Some(f) => out.push(f),
                None => return Decoded::Error,
            }
        }
        // QpackDecoder::OnDecodingCompleted.
        if ric > 0 {
            put_int(&mut self.pending, 0x80, 7, stream);
            self.known_received_count = self.known_received_count.max(ric);
        }
        if self.known_received_count < self.inserted() {
            let increment = self.inserted() - self.known_received_count;
            put_int(&mut self.pending, 0x00, 6, increment);
            self.known_received_count = self.inserted();
        }
        Decoded::Headers(out)
    }

    /// Decoder-stream bytes produced since the last call.
    pub(crate) fn take_decoder_stream(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }
}
