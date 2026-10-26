//! `core/webrtc/config.rs` -- an `RTCPeerConnection`'s `RTCConfiguration`: the dictionary conversion, Blink's
//! and libwebrtc's ICE-server checks, and `getConfiguration()` (a bot challenge hands its
//! STUN server to the constructor, and korvus's ignored the whole argument).
//!
//! Chrome 150.0.7871.46, measured with a Proxy over the dictionary and its server (and the messages over a
//! table of bad configurations, `tests/fixtures/webrtc/`):
//!
//! * the members are read in WebIDL order -- `alwaysNegotiateDataChannels`, `bundlePolicy`, `certificates`,
//!   `encodedInsertableStreams`, `iceCandidatePoolSize`, `iceServers` (each server: `credential`, `url`,
//!   `urls`, `username`), `iceTransportPolicy`, `iceTransports`, `rtcpMuxPolicy` -- korvus read none;
//! * `getConfiguration()` answers eight keys in that order, each server as `{credential, urls, username}`
//!   with `""` for an absent string and `urls` always a list -- korvus answered five keys, servers dropped;
//! * a server's URL is checked first by Blink (`rtc_peer_connection.cc`, `ParseConfiguration`: a valid URL,
//!   then stun/stuns without a query or turn/turns with `?transport=`, then TURN's username and credential),
//!   then by libwebrtc (`pc/ice_server_parsing.cc`: scheme, host, port, `user@host`, the transport value);
//! * `setConfiguration(c)` REPLACES the configuration -- a member it omits goes back to its default -- and
//!   refuses a change of `bundlePolicy`, `rtcpMuxPolicy` or `certificates`.

use deno_core::v8;

use crate::binding::instance_state as st;
use crate::binding::protos::{make_dom_exc, throw_type_error};

/// One `RTCIceServer`, normalized: `urls` always a list of URL strings, KURL-serialized.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct IceServer {
    pub urls: Vec<String>,
    pub username: String,
    pub credential: String,
}

/// The configuration a connection was given, converted.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Config {
    pub always_negotiate_data_channels: bool,
    pub bundle_policy: String,
    pub certificates: usize,
    pub encoded_insertable_streams: bool,
    pub ice_candidate_pool_size: u8,
    pub ice_servers: Vec<IceServer>,
    pub ice_transport_policy: String,
    pub rtcp_mux_policy: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            always_negotiate_data_channels: false,
            bundle_policy: "balanced".into(),
            certificates: 0,
            encoded_insertable_streams: false,
            ice_candidate_pool_size: 0,
            ice_servers: Vec::new(),
            ice_transport_policy: "all".into(),
            rtcp_mux_policy: "require".into(),
        }
    }
}

/// Where a conversion error is reported from: `Failed to construct 'RTCPeerConnection': ...` or
/// `Failed to execute 'setConfiguration' on 'RTCPeerConnection': ...`.
pub(crate) fn prefix(op: &str) -> String {
    if op == "construct" {
        "Failed to construct 'RTCPeerConnection': ".to_string()
    } else {
        format!("Failed to execute '{op}' on 'RTCPeerConnection': ")
    }
}

fn dom(scope: &mut v8::PinScope, msg: &str, name: &str) {
    if let Some(e) = make_dom_exc(scope, msg, name) {
        scope.throw_exception(e);
    }
}

/// A member as WebIDL reads it: a `[[Get]]` (getters run, in order); `undefined` is absent.
fn member<'s>(scope: &mut v8::PinScope<'s, '_>, d: Option<v8::Local<v8::Object>>, key: &str) -> Result<Option<v8::Local<'s, v8::Value>>, ()> {
    let Some(d) = d else { return Ok(None) };
    let k = crate::binding::protos::str_val(scope, key);
    v8::tc_scope!(let tc, scope);
    match d.get(tc, k) {
        Some(v) => Ok((!v.is_undefined()).then(|| v8::Local::new(tc, v))),
        None => {
            tc.rethrow();
            Err(())
        }
    }
}

fn read_enum(scope: &mut v8::PinScope, d: Option<v8::Local<v8::Object>>, key: &str, pre: &str, ty: &str, allowed: &[&str]) -> Result<Option<String>, ()> {
    let Some(v) = member(scope, d, key)? else { return Ok(None) };
    let Some(s) = v.to_string(scope) else { return Err(()) };
    let s = s.to_rust_string_lossy(scope);
    if allowed.contains(&s.as_str()) {
        return Ok(Some(s));
    }
    throw_type_error(scope, &format!("{pre}Failed to read the '{key}' property from 'RTCConfiguration': The provided value '{s}' is not a valid enum value of type {ty}."));
    Err(())
}

fn read_string(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Result<String, ()> {
    match v.to_string(scope) {
        Some(s) => Ok(s.to_rust_string_lossy(scope)),
        None => Err(()),
    }
}

/// A WebIDL sequence: a real array by index (Blink's fast path), anything else through its iterator -- the
/// realm's own `Array.from` runs the protocol, its throw leaving the call.
fn read_sequence<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'s, v8::Value>, pre: &str, key: &str) -> Result<Vec<v8::Local<'s, v8::Value>>, ()> {
    if let Ok(a) = v8::Local::<v8::Array>::try_from(v) {
        return Ok(st::array_items(scope, a));
    }
    let not_a_sequence = |scope: &mut v8::PinScope| {
        throw_type_error(scope, &format!("{pre}Failed to read the '{key}' property from 'RTCConfiguration': The provided value cannot be converted to a sequence."));
        Err(())
    };
    let Ok(o) = v8::Local::<v8::Object>::try_from(v) else { return not_a_sequence(scope) };
    let it = v8::Symbol::get_iterator(scope);
    let Some(f) = o.get(scope, it.into()) else { return Err(()) };
    if !f.is_function() {
        return not_a_sequence(scope);
    }
    let (Some(from), Some(recv)) = (crate::binding::protos::intrinsic_fn(scope, "Array.from"), crate::binding::protos::intrinsic(scope, "Array")) else {
        return not_a_sequence(scope);
    };
    let Some(r) = from.call(scope, recv, &[v]) else { return Err(()) };
    match v8::Local::<v8::Array>::try_from(r) {
        Ok(a) => Ok(st::array_items(scope, a)),
        Err(_) => not_a_sequence(scope),
    }
}

/// `RTCConfiguration`, converted and checked. `None`: an exception is pending.
pub(crate) fn parse(scope: &mut v8::PinScope, given: v8::Local<v8::Value>, op: &str) -> Option<Config> {
    let pre = prefix(op);
    let d = if given.is_null_or_undefined() {
        None
    } else if let Ok(o) = v8::Local::<v8::Object>::try_from(given) {
        Some(o)
    } else {
        throw_type_error(scope, &format!("{pre}The provided value is not of type 'RTCConfiguration'."));
        return None;
    };
    let mut c = Config::default();
    if let Some(v) = member(scope, d, "alwaysNegotiateDataChannels").ok()? {
        c.always_negotiate_data_channels = v.boolean_value(scope);
    }
    if let Some(s) = read_enum(scope, d, "bundlePolicy", &pre, "RTCBundlePolicy", &["balanced", "max-compat", "max-bundle"]).ok()? {
        c.bundle_policy = s;
    }
    if let Some(v) = member(scope, d, "certificates").ok()? {
        let items = read_sequence(scope, v, &pre, "certificates").ok()?;
        for it in &items {
            if !crate::install::webidl::is_instance_of(scope, *it, "RTCCertificate") {
                throw_type_error(scope, &format!("{pre}Failed to read the 'certificates' property from 'RTCConfiguration': Failed to convert value to 'RTCCertificate'."));
                return None;
            }
        }
        c.certificates = items.len();
    }
    if let Some(v) = member(scope, d, "encodedInsertableStreams").ok()? {
        c.encoded_insertable_streams = v.boolean_value(scope);
    }
    if let Some(v) = member(scope, d, "iceCandidatePoolSize").ok()? {
        // [EnforceRange] octet
        let n = v.number_value(scope)?;
        if !n.is_finite() {
            throw_type_error(scope, &format!("{pre}Failed to read the 'iceCandidatePoolSize' property from 'RTCConfiguration': Value is not of type 'octet'."));
            return None;
        }
        let t = n.trunc();
        if !(0.0..=255.0).contains(&t) {
            throw_type_error(scope, &format!("{pre}Failed to read the 'iceCandidatePoolSize' property from 'RTCConfiguration': Value is outside the 'octet' value range."));
            return None;
        }
        c.ice_candidate_pool_size = t as u8;
    }
    // Each server is converted here (its members read in order); the URL checks wait until every
    // member of the configuration has been read, as Blink's do.
    let mut raw_servers: Vec<(Option<Vec<String>>, Option<String>, Option<String>)> = Vec::new();
    if let Some(v) = member(scope, d, "iceServers").ok()? {
        for s in read_sequence(scope, v, &pre, "iceServers").ok()? {
            let so = if s.is_null_or_undefined() {
                None
            } else if let Ok(o) = v8::Local::<v8::Object>::try_from(s) {
                Some(o)
            } else {
                throw_type_error(scope, &format!("{pre}Failed to read the 'iceServers' property from 'RTCConfiguration': The provided value is not of type 'RTCIceServer'."));
                return None;
            };
            let credential = match member(scope, so, "credential").ok()? {
                Some(v) => Some(read_string(scope, v).ok()?),
                None => None,
            };
            let url = match member(scope, so, "url").ok()? {
                Some(v) => Some(read_string(scope, v).ok()?),
                None => None,
            };
            let urls = match member(scope, so, "urls").ok()? {
                // (DOMString or sequence<DOMString>): an object is the sequence, anything else a string
                Some(v) if v.is_object() => {
                    let mut out = Vec::new();
                    for it in read_sequence(scope, v, &pre, "urls").ok()? {
                        out.push(read_string(scope, it).ok()?);
                    }
                    Some(out)
                }
                Some(v) => Some(vec![read_string(scope, v).ok()?]),
                None => url.map(|u| vec![u]),
            };
            let username = match member(scope, so, "username").ok()? {
                Some(v) => Some(read_string(scope, v).ok()?),
                None => None,
            };
            raw_servers.push((urls, username, credential));
        }
    }
    let policy = read_enum(scope, d, "iceTransportPolicy", &pre, "RTCIceTransportPolicy", &["relay", "all"]).ok()?;
    let legacy = read_enum(scope, d, "iceTransports", &pre, "RTCIceTransportPolicy", &["relay", "all"]).ok()?;
    if let Some(p) = policy.or(legacy) {
        c.ice_transport_policy = p;
    }
    if let Some(s) = read_enum(scope, d, "rtcpMuxPolicy", &pre, "RTCRtcpMuxPolicy", &["negotiate", "require"]).ok()? {
        c.rtcp_mux_policy = s;
    }
    // Blink's checks, server by server, URL by URL.
    for (urls, username, credential) in &raw_servers {
        let Some(urls) = urls else {
            throw_type_error(scope, &format!("{pre}Malformed RTCIceServer"));
            return None;
        };
        let mut converted = Vec::new();
        for u in urls {
            let Ok(parsed) = url::Url::parse(u) else {
                dom(scope, &format!("{pre}'{u}' is not a valid URL."), "SyntaxError");
                return None;
            };
            let scheme = parsed.scheme();
            let query = parsed.query().unwrap_or("");
            let turn = (scheme == "turn" || scheme == "turns")
                && (query.is_empty() || {
                    let parts: Vec<&str> = query.split('=').filter(|p| !p.is_empty()).collect();
                    parts.len() >= 2 && parts[0] == "transport"
                });
            let stun = (scheme == "stun" || scheme == "stuns") && query.is_empty();
            if !turn && !stun {
                dom(scope, &format!("{pre}'{u}' is not a valid stun or turn URL."), "SyntaxError");
                return None;
            }
            if turn && (username.is_none() || credential.is_none()) {
                dom(scope, &format!("{pre}Both username and credential are required when the URL scheme is \"turn\" or \"turns\"."), "InvalidAccessError");
                return None;
            }
            converted.push(parsed.to_string());
        }
        c.ice_servers.push(IceServer { urls: converted, username: username.clone().unwrap_or_default(), credential: credential.clone().unwrap_or_default() });
    }
    // libwebrtc's, over what Blink hands it.
    for s in &c.ice_servers {
        if s.urls.is_empty() {
            dom(scope, &format!("{pre}ICE server parsing failed: Empty uri."), "SyntaxError");
            return None;
        }
        for u in &s.urls {
            if let Err((msg, name)) = libwebrtc_check(u, s) {
                dom(scope, &format!("{pre}ICE server parsing failed: {msg}"), name);
                return None;
            }
        }
    }
    Some(c)
}

/// A STUN server an ICE agent sends Binding Requests to: `(host, port, the URL as configured)`.
pub(crate) fn stun_servers(c: &Config) -> Vec<(String, u16, String)> {
    let mut out = Vec::new();
    for s in &c.ice_servers {
        for u in &s.urls {
            if let Ok((ServiceType::Stun, host, port)) = parse_ice_url(u) {
                out.push((host, port, u.clone()));
            }
        }
    }
    out
}

/// The UDP TURN servers of the configuration: `(host, port, url, username, credential)` for each
/// `turn:` URL whose transport is UDP (the default). `turns:` and `?transport=tcp` relays are not gathered.
pub(crate) fn turn_servers(c: &Config) -> Vec<(String, u16, String, String, String)> {
    let mut out = Vec::new();
    for s in &c.ice_servers {
        for u in &s.urls {
            let udp = u.starts_with("turn:") && !u.split_once('?').is_some_and(|(_, q)| q == "transport=tcp");
            if let (true, Ok((ServiceType::Turn, host, port))) = (udp, parse_ice_url(u)) {
                out.push((host, port, u.clone(), s.username.clone(), s.credential.clone()));
            }
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum ServiceType {
    Stun,
    Turn,
}

/// libwebrtc's `ParseIceServerUrl` (`pc/ice_server_parsing.cc`), as far as it decides an error.
fn libwebrtc_check(u: &str, s: &IceServer) -> Result<(), (&'static str, &'static str)> {
    let (ty, _, _) = parse_ice_url(u)?;
    if ty == ServiceType::Turn {
        if s.username.is_empty() || s.credential.is_empty() {
            return Err(("TURN server with empty username or password", "InvalidAccessError"));
        }
        if s.username.len() > 509 {
            return Err(("TURN server username is too long", "InvalidAccessError"));
        }
    }
    Ok(())
}

/// `scheme:host[:port][?transport=...]` -> the service, host and port (libwebrtc's defaults: 3478, 5349
/// for `turns`), or the error libwebrtc reports with the DOMException name Blink gives it.
fn parse_ice_url(u: &str) -> Result<(ServiceType, String, u16), (&'static str, &'static str)> {
    let tokens: Vec<&str> = u.split('?').collect();
    let base = tokens[0];
    let mut turn_tls = false;
    if tokens.len() == 2 {
        let t: Vec<&str> = tokens[1].split('=').collect();
        if t[0] != "transport" {
            return Err(("Invalid transport parameter key.", "SyntaxError"));
        }
        if t.len() < 2 {
            return Err(("Transport parameter missing value.", "SyntaxError"));
        }
        if t[1] != "udp" && t[1] != "tcp" {
            return Err(("Transport parameter should always be udp or tcp.", "SyntaxError"));
        }
    }
    let invalid = ("Invalid transport parameter in ICE URI", "SyntaxError");
    let colon = base.find(':').ok_or(invalid)?;
    if colon + 1 == base.len() {
        return Err(invalid);
    }
    let (ty, default_port) = match &base[..colon] {
        "stun" => (ServiceType::Stun, 3478),
        "stuns" => (ServiceType::Stun, 5349),
        "turn" => (ServiceType::Turn, 3478),
        "turns" => {
            turn_tls = true;
            (ServiceType::Turn, 5349)
        }
        _ => return Err(invalid),
    };
    let _ = turn_tls;
    if ty == ServiceType::Stun && tokens.len() > 1 {
        return Err(("Invalid stun url with query parameters", "SyntaxError"));
    }
    let hoststring = &base[colon + 1..];
    if hoststring.contains('@') {
        return Err(("Invalid url with long deprecated user@host syntax", "SyntaxError"));
    }
    let bad_host = ("Invalid hostname format", "SyntaxError");
    let digits = |s: &str| -> Result<i64, (&'static str, &'static str)> {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad_host);
        }
        s.parse::<i64>().map_err(|_| ("Invalid port", "SyntaxError"))
    };
    let (host, port) = if let Some(rest) = hoststring.strip_prefix('[') {
        let close = rest.rfind(']').ok_or(bad_host)?;
        let after = &rest[close + 1..];
        let port = if let Some(p) = after.strip_prefix(':') { digits(p)? } else { default_port };
        (rest[..close].to_string(), port)
    } else {
        let (h, port) = match hoststring.find(':') {
            Some(i) => (&hoststring[..i], digits(&hoststring[i + 1..])?),
            None => (hoststring, default_port),
        };
        // RFC 3986 reg-name characters
        if !h.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=%".contains(&b)) {
            return Err(bad_host);
        }
        (h.to_string(), port)
    };
    if host.is_empty() {
        return Err(bad_host);
    }
    if port <= 0 || port > 0xffff {
        return Err(("Invalid port", "SyntaxError"));
    }
    Ok((ty, host, port as u16))
}

/// `getConfiguration()`: Chrome's eight keys, in Chrome's order.
pub(crate) fn to_js<'s>(scope: &mut v8::PinScope<'s, '_>, c: &Config) -> v8::Local<'s, v8::Object> {
    let o = v8::Object::new(scope);
    st::set_bool(scope, o, "alwaysNegotiateDataChannels", c.always_negotiate_data_channels);
    st::set_str(scope, o, "bundlePolicy", &c.bundle_policy);
    let certs = st::array(scope, &[]);
    st::set(scope, o, "certificates", certs.into());
    st::set_bool(scope, o, "encodedInsertableStreams", c.encoded_insertable_streams);
    st::set_num(scope, o, "iceCandidatePoolSize", f64::from(c.ice_candidate_pool_size));
    let mut servers = Vec::new();
    for s in &c.ice_servers {
        let so = v8::Object::new(scope);
        st::set_str(scope, so, "credential", &s.credential);
        let urls: Vec<v8::Local<v8::Value>> = s.urls.iter().map(|u| crate::binding::protos::str_val(scope, u)).collect();
        let ua = st::array(scope, &urls);
        st::set(scope, so, "urls", ua.into());
        st::set_str(scope, so, "username", &s.username);
        servers.push(so.into());
    }
    let sa = st::array(scope, &servers);
    st::set(scope, o, "iceServers", sa.into());
    st::set_str(scope, o, "iceTransportPolicy", &c.ice_transport_policy);
    st::set_str(scope, o, "rtcpMuxPolicy", &c.rtcp_mux_policy);
    o
}

/// Kept in the connection's private state (no page code can reach it), in [`to_js`]'s shape.
pub(crate) fn store(scope: &mut v8::PinScope, pc_state: v8::Local<v8::Object>, c: &Config) {
    let o = to_js(scope, c);
    st::set(scope, pc_state, "config", o.into());
}

pub(crate) fn load(scope: &mut v8::PinScope, pc_state: v8::Local<v8::Object>) -> Config {
    let Ok(o) = v8::Local::<v8::Object>::try_from(st::get(scope, pc_state, "config")) else { return Config::default() };
    let mut c = Config {
        always_negotiate_data_channels: st::get_bool(scope, o, "alwaysNegotiateDataChannels"),
        bundle_policy: st::get_str(scope, o, "bundlePolicy"),
        certificates: 0,
        encoded_insertable_streams: st::get_bool(scope, o, "encodedInsertableStreams"),
        ice_candidate_pool_size: st::get_num(scope, o, "iceCandidatePoolSize") as u8,
        ice_servers: Vec::new(),
        ice_transport_policy: st::get_str(scope, o, "iceTransportPolicy"),
        rtcp_mux_policy: st::get_str(scope, o, "rtcpMuxPolicy"),
    };
    if let Some(servers) = st::get_array(scope, o, "iceServers") {
        for sv in st::array_items(scope, servers) {
            let Ok(so) = v8::Local::<v8::Object>::try_from(sv) else { continue };
            let urls = st::get_array(scope, so, "urls").map(|a| st::array_items(scope, a)).unwrap_or_default();
            let urls = urls.iter().map(|u| u.to_rust_string_lossy(scope)).collect();
            c.ice_servers.push(IceServer { urls, username: st::get_str(scope, so, "username"), credential: st::get_str(scope, so, "credential") });
        }
    }
    c
}
