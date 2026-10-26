//! korvus's license (goal 1020 step 6, the design's §7): the last token korvus's license server signed for this
//! machine, kept in
//! `<korvus home>/license`, and the key it was signed for in `<korvus home>/key` (`KORVUS_LICENSE_KEY` wins over it).
//!
//! A token is base64url(payload) "." base64url(an Ed25519 signature of the payload), the payload `{"v": 1, "license",
//! "install", "plan", "cap", "alloc", "seed_secret", "iat", "exp", "product"}`. korvus honours a token only when its
//! own key signed it AND it names korvus (`"product": "korvus"`): a token of another product -- or one naming none --
//! is refused even under korvus's key (invariant I10). An edited token fails its signature.
//!
//! Only the endpoint talks to the server (invariant I4), and it sends only what a heartbeat carries: the key, this
//! install's id, how many sessions are active and the version -- nothing about pages, URLs, proxies or personas.
//! When the server cannot be reached the last token holds until it expires; with no token korvus serves one session,
//! the free one. Nothing here stops a running session (I3): the cap only decides whether a new one waits.
//!
//!   POST <server>/v1/activate   {"key", "install", "version"}            -> 200 {"token"} | 403 {"error"}
//!   POST <server>/v1/activate   {"install", "version"}  (no key)         -> 200 {"token", "key"} | 429 {"error", "hint"}
//!   POST <server>/v1/heartbeat  {"key", "install", "active", "version"}  -> 200 {"token"} | 403 {"error"}

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use data_encoding::{BASE64URL_NOPAD, HEXLOWER};
use serde_json::{json, Value};

/// The public half of korvus's license-signing key (its seed is the license server's `SIGNING_KEY`, never here).
pub const PUBLIC_KEY: &str = "4f537780ebe92a651b310984ca70e6b8dfeaccdcbe42cb4ee3feba8e2e1a1a93";
/// The product a korvus token names.
pub const PRODUCT: &str = "korvus";
/// korvus's API host (decided 2026-10-21).
pub const DEFAULT_SERVER: &str = "https://api.korvus.dev";
/// How long one call to the server may take (an endpoint's first run waits this long for its free license offline).
const WITHIN: Duration = Duration::from_secs(10);

/// What a token grants this machine.
#[derive(Debug, Clone, PartialEq)]
pub struct License {
    /// The license id ("anon-..." for a machine's free license).
    pub id: String,
    pub plan: String,
    /// Sessions across the license's machines.
    pub cap: u64,
    /// This machine's share of them: how many sessions may be active here.
    pub alloc: u64,
    /// Unix seconds.
    pub expires: u64,
}

impl License {
    pub fn expired(&self) -> bool {
        now() >= self.expires
    }

    /// A machine's free license, with no account behind it.
    pub fn anonymous(&self) -> bool {
        self.id.starts_with("anon-")
    }
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default()
}

fn home() -> Result<PathBuf, String> {
    crate::settings::home().ok_or_else(|| "no home directory for the license".to_string())
}

pub fn token_path() -> Result<PathBuf, String> {
    Ok(home()?.join("license"))
}

/// The license `token` grants, if korvus honours it: signed by `public_hex`'s key, and korvus's.
pub fn verify_with(token: &str, public_hex: &str) -> Result<License, String> {
    let (payload, signature) = token.trim().split_once('.').ok_or("not a license token")?;
    let payload = BASE64URL_NOPAD.decode(payload.as_bytes()).map_err(|_| "not a license token")?;
    let signature = BASE64URL_NOPAD.decode(signature.as_bytes()).map_err(|_| "not a license token")?;
    let public = HEXLOWER.decode(public_hex.as_bytes()).map_err(|_| "a bad public key")?;
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, public)
        .verify(&payload, &signature)
        .map_err(|_| "the license token is not korvus's: its signature does not hold".to_string())?;
    let fields: Value = serde_json::from_slice(&payload).map_err(|_| "not a license token")?;
    // a token that names no product is treated as another product's, and refused
    let product = fields.get("product").and_then(Value::as_str).unwrap_or("");
    if product != PRODUCT {
        return Err(format!("the license token is not korvus's (product {product:?})"));
    }
    let number = |k: &str| fields.get(k).and_then(Value::as_u64);
    let text = |k: &str| fields.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    match (number("alloc"), number("exp")) {
        (Some(alloc), Some(expires)) => Ok(License { id: text("license"), plan: text("plan"), cap: number("cap").unwrap_or(alloc), alloc, expires }),
        _ => Err("the license token has no allocation or expiry".into()),
    }
}

/// [`verify_with`] korvus's own key.
pub fn verify(token: &str) -> Result<License, String> {
    verify_with(token, PUBLIC_KEY)
}

/// The license this machine holds, if it holds one korvus honours.
pub fn read() -> Result<License, String> {
    let path = token_path()?;
    let token = std::fs::read_to_string(&path).map_err(|e| format!("the license {}: {e}", path.display()))?;
    verify(&token).map_err(|e| format!("the license {}: {e}", path.display()))
}

/// This install's id: random, made once, kept in korvus's home.
pub fn install_id() -> Result<String, String> {
    let path = home()?.join("install");
    if let Ok(id) = std::fs::read_to_string(&path) {
        if !id.trim().is_empty() {
            return Ok(id.trim().to_string());
        }
    }
    let mut bytes = [0u8; 16];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes).map_err(|_| "no random bytes from the system".to_string())?;
    let id = HEXLOWER.encode(&bytes);
    std::fs::create_dir_all(home()?).map_err(|e| e.to_string())?;
    std::fs::write(&path, &id).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(id)
}

/// The license server: `KORVUS_SERVER`, else config.toml's `server`, else korvus's API host.
pub fn server() -> String {
    let configured = crate::settings::read_config().ok().and_then(|c| c.get("server").and_then(toml::Value::as_str).map(String::from));
    std::env::var("KORVUS_SERVER").ok().filter(|s| !s.is_empty()).or(configured).unwrap_or_else(|| DEFAULT_SERVER.to_string())
}

fn key_path() -> Result<PathBuf, String> {
    Ok(home()?.join("key"))
}

/// The key in force: `KORVUS_LICENSE_KEY` when set, else the one `korvus login` / `korvus activate` kept.
pub fn key() -> Option<String> {
    let kept = key_path().ok().and_then(|p| std::fs::read_to_string(p).ok()).map(|k| k.trim().to_string()).filter(|k| !k.is_empty());
    std::env::var("KORVUS_LICENSE_KEY").ok().filter(|k| !k.is_empty()).or(kept)
}

pub fn keep_key(key: &str) -> Result<(), String> {
    let path = key_path()?;
    std::fs::create_dir_all(home()?).map_err(|e| e.to_string())?;
    std::fs::write(&path, key).map_err(|e| format!("{}: {e}", path.display()))
}

/// Forgets the account: the kept key and the token both go.
pub fn forget() -> Result<(), String> {
    let _ = token_path().map(std::fs::remove_file);
    let _ = key_path().map(std::fs::remove_file);
    Ok(())
}

/// Writes `token` where korvus reads it, whole or not at all -- once korvus honours it.
fn store_token(token: &str) -> Result<License, String> {
    let license = verify(token)?;
    let path = token_path()?;
    std::fs::create_dir_all(home()?).map_err(|e| e.to_string())?;
    let part = path.with_extension("new");
    std::fs::write(&part, token).and_then(|()| std::fs::rename(&part, &path)).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(license)
}

fn post(server: &str, endpoint: &str, body: &Value) -> Result<(u16, Value), String> {
    let url = format!("{}/v1/{endpoint}", server.trim_end_matches('/'));
    let (status, bytes) = transport::http_request("POST", &url, &[("accept", "application/json")], Some(("application/json", body.to_string().into_bytes())), None, WITHIN)?;
    Ok((status, serde_json::from_slice(&bytes).unwrap_or(Value::Null)))
}

/// What a heartbeat brought.
pub enum Beat {
    Renewed(License),
    Revoked(String),
}

fn call(server: &str, endpoint: &str, mut body: Value) -> Result<Beat, String> {
    body["install"] = install_id()?.into();
    body["version"] = crate::VERSION.into();
    let (status, answer) = post(server, endpoint, &body)?;
    match (status, answer["token"].as_str()) {
        (200, Some(token)) => store_token(token).map(Beat::Renewed),
        (403, _) => Ok(Beat::Revoked(answer["error"].as_str().unwrap_or("refused").to_string())),
        _ => Err(format!("{server}: answered {status}")),
    }
}

/// `korvus activate <key>`: keeps the key and writes its first token.
pub fn activate(server: &str, key: &str) -> Result<License, String> {
    match call(server, "activate", json!({ "key": key }))? {
        Beat::Renewed(license) => {
            keep_key(key)?;
            Ok(license)
        }
        Beat::Revoked(why) => Err(format!("the server refused the key: {why}")),
    }
}

/// A machine's free license, for an install with no key: the server's key is kept, the token written.
pub fn activate_anonymously(server: &str) -> Result<License, String> {
    let body = json!({ "install": install_id()?, "version": crate::VERSION });
    let (status, answer) = post(server, "activate", &body)?;
    match status {
        200 => {
            let token = answer["token"].as_str().ok_or("the server sent no token")?;
            let key = answer["key"].as_str().ok_or("the server sent no key")?;
            let license = store_token(token)?;
            keep_key(key)?;
            Ok(license)
        }
        429 => Err(format!("{}\n{}", answer["error"].as_str().unwrap_or("too many free licenses from this address"), answer["hint"].as_str().unwrap_or_default())),
        403 => Err(answer["error"].as_str().unwrap_or("refused").to_string()),
        other => Err(format!("{server}: answered {other}")),
    }
}

/// One heartbeat for `active` sessions. A revoked license's token goes, so new sessions wait.
pub fn heartbeat(server: &str, key: &str, active: usize) -> Result<Beat, String> {
    let beat = call(server, "heartbeat", json!({ "key": key, "active": active }))?;
    if let Beat::Revoked(_) = beat {
        let _ = token_path().map(std::fs::remove_file);
    }
    Ok(beat)
}

/// The license a starting endpoint works under: the token kept, renewed when it has expired; a first run's free
/// license, activated; or, when none can be had, the one free session korvus serves without a server.
pub fn ensure(server: &str) -> (Option<License>, Option<String>) {
    match (read(), key()) {
        (Ok(l), _) if !l.expired() => (Some(l), None),
        (_, Some(k)) => match call(server, "activate", json!({ "key": k })) {
            Ok(Beat::Renewed(l)) => (Some(l), None),
            Ok(Beat::Revoked(why)) => (None, Some(format!("the server refused this machine's key: {why}"))),
            Err(e) => (None, Some(e)),
        },
        (_, None) => match activate_anonymously(server) {
            Ok(l) => (Some(l), None),
            Err(e) => (None, Some(e)),
        },
    }
}

/// The sessions a license lets this machine run at once: its allocation while it holds, else the free one.
pub fn sessions(license: Option<&License>) -> usize {
    match license {
        Some(l) if !l.expired() => l.alloc as usize,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // goal 1020 invariant I10, korvus's side: a korvus token is honoured only under korvus's key; a token of another
    // product (or one naming none) is refused under either key. One grant (license L, install I, plan p, cap 3,
    // alloc 2, iat 10, exp 20) signed by each test key; korvus's names product "korvus", the other names none.
    const KORVUS_KEY: &str = "a09aa5f47a6759802ff955f8dc2d2a14a5c99d23be97f864127ff9383455a4f0";
    const OTHER_KEY: &str = "4a178fddbfea15c2acc2a8aa45a0e11cc11a7c8ab8dce304e7955c3625824240";
    const KORVUS_TOKEN: &str = "eyJhbGxvYyI6MiwiY2FwIjozLCJleHAiOjIwLCJpYXQiOjEwLCJpbnN0YWxsIjoiSSIsImxpY2Vuc2UiOiJMIiwicGxhbiI6InAiLCJwcm9kdWN0Ijoia29ydnVzIiwic2VlZF9zZWNyZXQiOiJzIiwidiI6MX0.12Ciepm2zb2deVloCWNMORXTD1Tc1-LpsD_TGGT9UO28rwNeextqpsZ8hnZ_upnVBdV6dYV_sEWipuvt8Om8Dw";
    const OTHER_TOKEN: &str = "eyJhbGxvYyI6MiwiY2FwIjozLCJleHAiOjIwLCJpYXQiOjEwLCJpbnN0YWxsIjoiSSIsImxpY2Vuc2UiOiJMIiwicGxhbiI6InAiLCJzZWVkX3NlY3JldCI6InMiLCJ2IjoxfQ.klaBl0Q0QQSRIPlcjKTFbY7PTmW76FGsDiD6jk7ZE3ma-1s-tTYWjKVBzptmQOsOBwmg5GpzmrKCwpimTVoNDw";

    #[test]
    fn korvus_honours_its_own_tokens_only() {
        let l = verify_with(KORVUS_TOKEN, KORVUS_KEY).unwrap();
        assert_eq!((l.alloc, l.cap, l.plan.as_str()), (2, 3, "p"));
        assert!(verify_with(OTHER_TOKEN, OTHER_KEY).unwrap_err().contains("not korvus's"));
        assert!(verify_with(OTHER_TOKEN, KORVUS_KEY).is_err());
        assert!(verify_with(KORVUS_TOKEN, OTHER_KEY).is_err());
        let edited = format!("{}x{}", &KORVUS_TOKEN[..10], &KORVUS_TOKEN[11..]);
        assert!(verify_with(&edited, KORVUS_KEY).is_err());
    }

    #[test]
    fn with_no_license_one_session() {
        assert_eq!(sessions(None), 1);
        let l = License { id: "L".into(), plan: "p".into(), cap: 5, alloc: 4, expires: now() + 60 };
        assert_eq!(sessions(Some(&l)), 4);
        assert_eq!(sessions(Some(&License { expires: 1, ..l })), 1);
    }
}
