//! Encrypted Client Hello from the host's DNS `HTTPS` record.
//!
//! # Why
//!
//! Chrome resolves a host's `HTTPS` record (RFC 9460, type 65) beside its addresses, and when the
//! record carries an `ech` SvcParam it offers REAL Encrypted Client Hello: the true server name rides
//! in the encrypted inner ClientHello and the outer one names only the config's `public_name`. This
//! engine sent ECH GREASE only, so on a network that filters TLS by SNI a host Chrome loads had every
//! connection reset: measured 2026-09-26 over the Tranco 400, 13 hosts publish an `ech` config and one
//! of them, `beastiality.tv`, resets a plain-SNI handshake here (`curl: (35) Recv failure`) while the
//! paired Chrome row loads it.
//!
//! # How
//!
//! [`prepare`] runs before a request is sent: one `HTTPS` query to the system resolver (the first
//! `nameserver` of `/etc/resolv.conf`, UDP with EDNS0, TCP when the answer is truncated), and the
//! `ech` parameter of the first service-mode record, cached for the record's TTL (at most five
//! minutes; a host with no config is remembered as such for as long). The vendored `wreq`'s connect
//! path asks [`cached`] for the host it is about to handshake with (`vendor/wreq/src/tls.rs`,
//! `KORVUS PATCH`) and hands the bytes to BoringSSL.
//!
//! # Where it stays out of the way
//!
//! No lookup is made for an IP literal, a single-label or `.localhost`/`.test` host (the in-process test
//! servers), or when a proxy is configured through the environment: Chrome behind an HTTP proxy
//! resolves nothing itself and so has no ECH config either -- and a replay, which always runs through
//! its recording proxy, stays off the live DNS. A lookup that fails or times out answers "no config",
//! and the connection is made exactly as before.

use std::collections::HashMap;
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How long one lookup may take before the request goes ahead without ECH.
const LOOKUP_TIMEOUT: Duration = Duration::from_millis(800);
/// The longest a cached answer is trusted, whatever the record's TTL says.
const MAX_TTL: Duration = Duration::from_secs(300);

struct Entry {
    config: Option<Vec<u8>>,
    /// The record's `alpn` list (H3, 2026-10-11): `h3` in it starts Chrome's DNS_ALPN_H3 job, QUIC on the first
    /// request (`http_stream_factory_job_controller.cc:917-930`).
    alpn: Option<Vec<String>>,
    until: Instant,
}

fn cache() -> &'static Mutex<HashMap<String, Entry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The `ECHConfigList` [`prepare`] found for `host`, if a fresh one is cached. The provider the
/// vendored `wreq` calls on its connect path; it never blocks.
pub fn cached(host: &str) -> Option<Vec<u8>> {
    let c = cache().lock().ok()?;
    let e = c.get(&host.to_ascii_lowercase())?;
    if e.until < Instant::now() {
        return None;
    }
    e.config.clone()
}

/// The `alpn` list of `host`'s `HTTPS` record, if a fresh answer is cached (H3: the provider the vendored `wreq`'s
/// HTTP/3 side asks before racing QUIC on a host it has no Alt-Svc for). Never blocks.
pub fn cached_alpn(host: &str) -> Option<Vec<String>> {
    let c = cache().lock().ok()?;
    let e = c.get(&host.to_ascii_lowercase())?;
    if e.until < Instant::now() {
        return None;
    }
    e.alpn.clone()
}

/// Would Chrome look `host` up for ECH at all? See the module note.
fn eligible(host: &str) -> bool {
    if host.parse::<std::net::IpAddr>().is_ok() || host.starts_with('[') || !host.contains('.') {
        return false;
    }
    let h = host.trim_end_matches('.').to_ascii_lowercase();
    if h.ends_with(".localhost") || h.ends_with(".test") || h == "localhost" {
        return false;
    }
    !["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"]
        .iter()
        .any(|k| std::env::var(k).is_ok_and(|v| !v.is_empty()))
}

/// Resolve and cache `host`'s ECH config, if it has one and it is not already cached. Called before a
/// request to an `https:` URL is sent.
pub async fn prepare(host: &str) {
    if !eligible(host) {
        return;
    }
    let key = host.to_ascii_lowercase();
    if let Ok(c) = cache().lock() {
        if c.get(&key).is_some_and(|e| e.until >= Instant::now()) {
            return;
        }
    }
    let name = key.clone();
    let looked = tokio::task::spawn_blocking(move || lookup(&name));
    let (config, alpn, ttl) = match tokio::time::timeout(LOOKUP_TIMEOUT, looked).await {
        Ok(Ok(Some((config, alpn, ttl)))) => (config, alpn, ttl),
        // Failed or too slow: go ahead without, and ask again next time.
        _ => return,
    };
    if let Ok(mut c) = cache().lock() {
        c.insert(key, Entry { config, alpn, until: Instant::now() + ttl.min(MAX_TTL) });
    }
}

/// The first `nameserver` in `/etc/resolv.conf`.
fn resolver() -> Option<SocketAddr> {
    let text = std::fs::read_to_string("/etc/resolv.conf").ok()?;
    let ip = text.lines().find_map(|l| {
        let mut w = l.split_whitespace();
        (w.next() == Some("nameserver")).then(|| w.next()).flatten()
    })?;
    let ip: std::net::IpAddr = ip.parse().ok()?;
    Some(SocketAddr::new(ip, 53))
}

/// One `HTTPS` query: `Some((config, alpn, ttl))` when the resolver answered (config `None` when the host
/// publishes no ECH config, alpn `None` when no record lists one), `None` when it did not.
fn lookup(host: &str) -> Option<(Option<Vec<u8>>, Option<Vec<String>>, Duration)> {
    let server = resolver()?;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    let id: u16 = (nanos as u16) ^ ((nanos >> 16) as u16) ^ (std::process::id() as u16);
    let query = build_query(id, host)?;
    let sock = UdpSocket::bind(if server.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }).ok()?;
    sock.set_read_timeout(Some(LOOKUP_TIMEOUT)).ok()?;
    sock.send_to(&query, server).ok()?;
    let mut buf = vec![0u8; 4096];
    let n = loop {
        let (n, from) = sock.recv_from(&mut buf).ok()?;
        if from == server && n >= 12 && u16::from_be_bytes([buf[0], buf[1]]) == id {
            break n;
        }
    };
    let mut answer = buf[..n].to_vec();
    // Truncated: ask again over TCP, where the whole answer fits.
    if answer[2] & 0x02 != 0 {
        answer = tcp_query(server, &query)?;
    }
    let (config, ttl) = parse_response(&answer, id)?;
    Some((config, parse_alpn(&answer), ttl))
}

/// The `alpn` SvcParam (key 1) of the first service-mode `HTTPS` record in an answer that carries one (H3).
fn parse_alpn(msg: &[u8]) -> Option<Vec<String>> {
    let qd = u16::from_be_bytes([*msg.get(4)?, *msg.get(5)?]) as usize;
    let an = u16::from_be_bytes([*msg.get(6)?, *msg.get(7)?]) as usize;
    let mut at = 12;
    for _ in 0..qd {
        at = skip_name(msg, at)? + 4;
    }
    for _ in 0..an {
        at = skip_name(msg, at)?;
        let ty = u16::from_be_bytes([*msg.get(at)?, *msg.get(at + 1)?]);
        let rdlen = u16::from_be_bytes([*msg.get(at + 8)?, *msg.get(at + 9)?]) as usize;
        let rdata = msg.get(at + 10..at + 10 + rdlen)?;
        at += 10 + rdlen;
        if ty != 65 {
            continue;
        }
        if let Some(value) = svcb_param(rdata, 1) {
            // A sequence of length-prefixed protocol ids (RFC 9460 7.1.1).
            let mut out = Vec::new();
            let mut i = 0;
            while i < value.len() {
                let n = value[i] as usize;
                out.push(String::from_utf8_lossy(value.get(i + 1..i + 1 + n)?).into_owned());
                i += 1 + n;
            }
            return Some(out);
        }
    }
    None
}

fn tcp_query(server: SocketAddr, query: &[u8]) -> Option<Vec<u8>> {
    use std::io::{Read, Write};
    let mut s = TcpStream::connect_timeout(&server, LOOKUP_TIMEOUT).ok()?;
    s.set_read_timeout(Some(LOOKUP_TIMEOUT)).ok()?;
    let mut msg = (query.len() as u16).to_be_bytes().to_vec();
    msg.extend_from_slice(query);
    s.write_all(&msg).ok()?;
    let mut len = [0u8; 2];
    s.read_exact(&mut len).ok()?;
    let mut out = vec![0u8; u16::from_be_bytes(len) as usize];
    s.read_exact(&mut out).ok()?;
    Some(out)
}

/// A recursive query for `host`'s `HTTPS` record, with an EDNS0 OPT record offering 1232-byte answers.
fn build_query(id: u16, host: &str) -> Option<Vec<u8>> {
    let mut q = Vec::with_capacity(64);
    q.extend_from_slice(&id.to_be_bytes());
    q.extend_from_slice(&[0x01, 0x00]); // RD
    q.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 1]); // QD 1, AN 0, NS 0, AR 1
    for label in host.trim_end_matches('.').split('.') {
        if label.is_empty() || label.len() > 63 {
            return None;
        }
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&[0, 65, 0, 1]); // QTYPE HTTPS, QCLASS IN
    q.extend_from_slice(&[0, 0, 41, 0x04, 0xd0, 0, 0, 0, 0, 0, 0]); // OPT, 1232 bytes, no options
    Some(q)
}

/// Skip a (possibly compressed) name at `at`; the offset just past it.
fn skip_name(msg: &[u8], mut at: usize) -> Option<usize> {
    loop {
        let len = *msg.get(at)? as usize;
        if len == 0 {
            return Some(at + 1);
        }
        if len & 0xc0 == 0xc0 {
            return Some(at + 2);
        }
        at += 1 + len;
    }
}

/// The `ech` SvcParam of the first service-mode `HTTPS` record in the answer section, with the
/// record's TTL; `(None, ttl)` for an answer with no such record (the negative answer's TTL is the
/// SOA minimum in principle -- five minutes is used, the cache's own ceiling).
fn parse_response(msg: &[u8], id: u16) -> Option<(Option<Vec<u8>>, Duration)> {
    if msg.len() < 12 || u16::from_be_bytes([msg[0], msg[1]]) != id || msg[2] & 0x80 == 0 {
        return None;
    }
    let rcode = msg[3] & 0x0f;
    if rcode != 0 && rcode != 3 {
        return None; // SERVFAIL, REFUSED, ...: no answer to cache
    }
    let qd = u16::from_be_bytes([msg[4], msg[5]]) as usize;
    let an = u16::from_be_bytes([msg[6], msg[7]]) as usize;
    let mut at = 12;
    for _ in 0..qd {
        at = skip_name(msg, at)? + 4;
    }
    for _ in 0..an {
        at = skip_name(msg, at)?;
        let ty = u16::from_be_bytes([*msg.get(at)?, *msg.get(at + 1)?]);
        let ttl = u32::from_be_bytes([*msg.get(at + 4)?, *msg.get(at + 5)?, *msg.get(at + 6)?, *msg.get(at + 7)?]);
        let rdlen = u16::from_be_bytes([*msg.get(at + 8)?, *msg.get(at + 9)?]) as usize;
        let rdata = msg.get(at + 10..at + 10 + rdlen)?;
        at += 10 + rdlen;
        if ty != 65 {
            continue; // a CNAME on the way to the record
        }
        if let Some(ech) = svcb_ech(rdata) {
            return Some((Some(ech), Duration::from_secs(ttl as u64)));
        }
    }
    Some((None, MAX_TTL))
}

/// The `ech` parameter (key 5) of one SVCB/HTTPS RDATA, when it is a service-mode record.
fn svcb_ech(rdata: &[u8]) -> Option<Vec<u8>> {
    svcb_param(rdata, 5)
}

/// SvcParam `want` of one SVCB/HTTPS RDATA, when it is a service-mode record.
fn svcb_param(rdata: &[u8], want: u16) -> Option<Vec<u8>> {
    let priority = u16::from_be_bytes([*rdata.first()?, *rdata.get(1)?]);
    if priority == 0 {
        return None; // alias mode: no parameters
    }
    // The target name is never compressed (RFC 9460 §2.2).
    let mut at = 2;
    loop {
        let len = *rdata.get(at)? as usize;
        at += 1 + len;
        if len == 0 {
            break;
        }
    }
    while at + 4 <= rdata.len() {
        let key = u16::from_be_bytes([rdata[at], rdata[at + 1]]);
        let len = u16::from_be_bytes([rdata[at + 2], rdata[at + 3]]) as usize;
        let value = rdata.get(at + 4..at + 4 + len)?;
        if key == want {
            return Some(value.to_vec());
        }
        at += 4 + len;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A response built by hand: one CNAME-free `HTTPS` answer with `alpn` and `ech`.
    fn response(id: u16, ech: &[u8], ttl: u32) -> Vec<u8> {
        let mut m = id.to_be_bytes().to_vec();
        m.extend_from_slice(&[0x81, 0x80, 0, 1, 0, 1, 0, 0, 0, 0]);
        m.extend_from_slice(b"\x07example\x03com\x00\x00\x41\x00\x01");
        m.extend_from_slice(&[0xc0, 0x0c, 0, 65, 0, 1]);
        m.extend_from_slice(&ttl.to_be_bytes());
        let mut rd = vec![0, 1, 0]; // priority 1, target "."
        rd.extend_from_slice(&[0, 1, 0, 3, 2, b'h', b'2']); // alpn=h2
        rd.extend_from_slice(&[0, 5]);
        rd.extend_from_slice(&(ech.len() as u16).to_be_bytes());
        rd.extend_from_slice(ech);
        m.extend_from_slice(&(rd.len() as u16).to_be_bytes());
        m.extend_from_slice(&rd);
        m
    }

    #[test]
    fn the_ech_parameter_is_read_out_of_an_https_answer() {
        let r = response(7, &[0xfe, 0x0d, 1, 2, 3], 60);
        assert_eq!(parse_response(&r, 7), Some((Some(vec![0xfe, 0x0d, 1, 2, 3]), Duration::from_secs(60))));
        assert_eq!(parse_response(&r, 8), None, "another query's answer");
        // H3: the same record's `alpn`.
        assert_eq!(parse_alpn(&r), Some(vec!["h2".to_string()]));
    }

    #[test]
    fn the_query_asks_for_https_with_edns() {
        let q = build_query(1, "example.com").unwrap();
        assert!(q.windows(13).any(|w| w == b"\x07example\x03com\x00"));
        assert_eq!(&q[q.len() - 15..q.len() - 11], &[0, 65, 0, 1]);
    }

    #[test]
    fn loopback_and_test_hosts_are_not_looked_up() {
        for h in ["127.0.0.1", "localhost", "web-platform.test", "a.localhost", "[::1]"] {
            assert!(!eligible(h), "{h}");
        }
    }
}
