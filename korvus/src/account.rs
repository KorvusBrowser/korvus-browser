//! The account commands (goal 1020 step 6):
//!
//!   korvus login            GitHub's device flow, then korvus's `/v1/cli/github`: the account's key, kept, activated
//!   korvus logout           back to this machine's free session
//!   korvus status           the plan, the sessions in use across machines, this install
//!   korvus activate <key>   a key from the account page, kept and activated
//!
//! The GitHub token reaches only `/v1/cli/github` and is never written. The GitHub OAuth app is KorvusBrowser's (D19):
//! its client id is built in (`KORVUS_GITHUB_CLIENT_ID` at build time, or in the environment); `KORVUS_GITHUB_URL`
//! points the flow at a stand-in GitHub for the probes.

use std::io::IsTerminal;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::license;

const WITHIN: Duration = Duration::from_secs(20);

fn github_base() -> String {
    std::env::var("KORVUS_GITHUB_URL").ok().filter(|u| !u.is_empty()).unwrap_or_else(|| "https://github.com".into())
}

fn client_id() -> Option<String> {
    std::env::var("KORVUS_GITHUB_CLIENT_ID").ok().filter(|c| !c.is_empty()).or_else(|| option_env!("KORVUS_GITHUB_CLIENT_ID").map(String::from))
}

fn form(url: &str, body: &str) -> Result<Value, String> {
    let (_, bytes) = transport::http_request("POST", url, &[("accept", "application/json")], Some(("application/x-www-form-urlencoded", body.as_bytes().to_vec())), None, WITHIN)?;
    serde_json::from_slice(&bytes).map_err(|_| format!("{url}: not JSON"))
}

/// The platform's envelope (`{"success", "data"}` / `{"success": false, "error": {"code", "message"}}`).
fn answer(status: u16, body: &Value) -> Result<Value, String> {
    if body["success"].as_bool() == Some(true) {
        return Ok(body["data"].clone());
    }
    let message = body["error"]["message"].as_str().unwrap_or("the server refused");
    Err(match body["error"]["code"].as_str().unwrap_or_default() {
        "GITHUB_AUTH_FAILED" => "GitHub did not accept the sign-in: run \"korvus login\" again".into(),
        "GITHUB_NO_VERIFIED_EMAIL" => "your GitHub account has no verified email: add one at https://github.com/settings/emails, then run \"korvus login\"".into(),
        _ if status == 429 => "too many sign-ins from this address: try again in a few minutes".into(),
        _ => message.to_string(),
    })
}

fn platform(method: &str, path: &str, bearer: Option<&str>, body: Option<&Value>) -> Result<Value, String> {
    let url = format!("{}{path}", license::server().trim_end_matches('/'));
    let auth = bearer.map(|k| format!("Bearer {k}"));
    let mut headers = vec![("accept", "application/json")];
    if let Some(a) = &auth {
        headers.push(("authorization", a.as_str()));
    }
    let (status, bytes) = transport::http_request(method, &url, &headers, body.map(|b| ("application/json", b.to_string().into_bytes())), None, WITHIN)?;
    answer(status, &serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

pub fn login() -> Result<(), String> {
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        return Err("korvus login needs a terminal; set KORVUS_LICENSE_KEY, or run `korvus activate <key>`".into());
    }
    let id = client_id().ok_or("this korvus was built without a GitHub client id (KORVUS_GITHUB_CLIENT_ID)")?;
    let base = github_base();
    let code = form(&format!("{base}/login/device/code"), &format!("client_id={id}&scope=read%3Auser%20user%3Aemail"))?;
    let device = code["device_code"].as_str().ok_or("GitHub sent no device code")?.to_string();
    println!("  Open {} and enter:  {}", code["verification_uri"].as_str().unwrap_or("https://github.com/login/device"), code["user_code"].as_str().unwrap_or("?"));
    println!("  (any device works: your phone too. Waiting…)");
    let deadline = Instant::now() + Duration::from_secs(code["expires_in"].as_u64().unwrap_or(900));
    let mut interval = code["interval"].as_u64().unwrap_or(5);
    let token = loop {
        if Instant::now() >= deadline {
            return Err("the code expired: run korvus login again".into());
        }
        let poll = form(&format!("{base}/login/oauth/access_token"), &format!("client_id={id}&device_code={device}&grant_type=urn:ietf:params:oauth:grant-type:device_code"))?;
        if let Some(t) = poll["access_token"].as_str() {
            break t.to_string();
        }
        match poll["error"].as_str() {
            Some("authorization_pending") => {}
            Some("slow_down") => interval = poll["interval"].as_u64().unwrap_or(interval + 5),
            Some("expired_token") => return Err("the code expired: run korvus login again".into()),
            Some("access_denied") => return Err("the sign-in was declined on GitHub".into()),
            other => return Err(poll["error_description"].as_str().or(other).unwrap_or("GitHub refused").to_string()),
        }
        std::thread::sleep(Duration::from_secs(interval));
    };
    let account = platform("POST", "/v1/cli/github", None, Some(&json!({ "token": token, "install": license::install_id()?, "product": license::PRODUCT })))?;
    let key = account["key"].as_str().ok_or("the server sent no key")?;
    let l = license::activate(&license::server(), key)?;
    let who = account["login"].as_str().unwrap_or("?");
    if account["created"].as_bool() == Some(true) {
        println!("✓ Signed up as @{who}. {} sessions ({}).", l.cap, l.plan);
    } else {
        println!("✓ Signed in as @{who}. {} sessions ({}).", l.cap, l.plan);
    }
    if std::env::var("KORVUS_LICENSE_KEY").is_ok_and(|v| !v.is_empty()) {
        println!("KORVUS_LICENSE_KEY is set and still wins over this key");
    }
    Ok(())
}

pub fn logout() -> Result<(), String> {
    if std::env::var("KORVUS_LICENSE_KEY").is_ok_and(|v| !v.is_empty()) {
        return Err("KORVUS_LICENSE_KEY is set: unset it to log out".into());
    }
    if license::read().map(|l| l.anonymous()).unwrap_or(true) {
        return Err("not signed in".into());
    }
    license::forget()?;
    license::activate_anonymously(&license::server())?;
    println!("Logged out: 1 session on this machine.");
    Ok(())
}

pub fn status() -> Result<(), String> {
    let install = license::install_id()?;
    let held = license::read().ok();
    let anonymous = held.as_ref().map(|l| l.anonymous()).unwrap_or(true);
    match license::key().filter(|_| !anonymous).map(|k| platform("GET", "/v1/cli/status", Some(&k), None)) {
        Some(Ok(d)) => {
            println!("plan {}: {} sessions, {} active across {} machine(s)", d["plan"].as_str().unwrap_or("?"), d["cap"], d["active"].as_u64().unwrap_or(0), d["machines"].as_array().map(Vec::len).unwrap_or(0));
        }
        Some(Err(e)) => println!("the account's status did not answer ({e}); this machine's license below"),
        None => println!("no account: this machine's free session (korvus login for a plan)"),
    }
    match held {
        Some(l) => println!("this machine: {} of {} sessions, license {} ({}), until {}", l.alloc, l.cap, l.id, l.plan, l.expires),
        None => println!("this machine: no license yet -- 1 session; korvus serve activates one"),
    }
    println!("install {install}");
    Ok(())
}

pub fn activate(key: &str) -> Result<(), String> {
    let l = license::activate(&license::server(), key)?;
    println!("✓ Activated: {} of {} sessions on this machine ({}).", l.alloc, l.cap, l.plan);
    Ok(())
}
