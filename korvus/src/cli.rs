//! The commands that run no page (goal 1020 step 5, the design's §8):
//!
//!   korvus setup [--non-interactive]           the defaults: "Stealth by default?", then "Rotate the persona for each
//!                                              new session?"; never asked without a terminal, in CI or a container
//!                                              (stealth and rotate there)
//!   korvus persona new <name> [--from <seed:<os>:<x> | <document.json>>]
//!   korvus persona set <name> <key>=<value>... proxy, timezone, locale, humanize: the persona's session
//!   korvus persona ls | rm <name>              named personas, under <korvus home>/personas/<name>/
//!   korvus persona reset <name>                drop its profile (cookies, storage): the next context starts clean
//!   korvus persona export <name> [file]        its profile as JSON (every slot, and its cookies as DevTools reads them)
//!   korvus persona import <name> <file>        a file `export` wrote, as its profile
//!   korvus plugin ls                           the plugins a run loads, and the slots each fills
//!   korvus plugin check <path>                 load one plugin library and print what it declares
//!   korvus stats [--port 9222]                 a running `korvus serve`'s active sessions
//!
//! A named persona is a persona document (`persona.json`) and, once a context has worn it, its profile (`profile/`):
//! the cookies and storage its pages kept, which the next context of that name starts with.

use std::io::{BufRead, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::settings::{self, Settings};

fn fail(msg: impl std::fmt::Display) -> i32 {
    eprintln!("korvus: {msg}");
    2
}

// ── setup ────────────────────────────────────────────────────────────────────────────────────────

/// A person can be asked: both ends are a terminal, and this is not CI or a container.
fn can_ask() -> bool {
    let ci = std::env::var("CI").is_ok_and(|v| !v.is_empty() && v != "0" && v != "false");
    !ci && !in_container() && std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// A container: Docker's or Podman's marker file, systemd's `container`, or a container cgroup for PID 1.
fn in_container() -> bool {
    Path::new("/.dockerenv").exists()
        || Path::new("/run/.containerenv").exists()
        || std::env::var_os("container").is_some()
        || std::fs::read_to_string("/proc/1/cgroup").is_ok_and(|c| ["docker", "kubepods", "containerd", "lxc", "podman"].iter().any(|k| c.contains(k)))
}

/// One yes/no question, yes the default.
fn ask(question: &str) -> bool {
    print!("{question} [Y/n] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    !matches!(line.trim().to_ascii_lowercase().as_str(), "n" | "no")
}

/// A seed no other install draws: the clock and the process, mixed.
fn fresh_seed() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    h.write_u32(std::process::id());
    h.finish()
}

pub fn setup(args: &[String]) -> i32 {
    let asked = !args.iter().any(|a| a == "--non-interactive") && can_ask();
    let (stealth, rotate) = if asked {
        let stealth = ask("Stealth by default?");
        (stealth, stealth && ask("Rotate the persona for each new session?"))
    } else {
        (true, true)
    };
    let Some(path) = settings::config_path() else { return fail("no home directory for the config file") };
    let mut config = match settings::read_config() {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    config.insert("identity".into(), toml::Value::String(if stealth { "stealth" } else { "korvus" }.into()));
    // a seed in the config is every context's: only `fixed` keeps one
    config.remove("seed");
    if stealth {
        config.insert("rotation".into(), toml::Value::String(if rotate { "rotate" } else { "fixed" }.into()));
        if !rotate {
            config.insert("seed".into(), toml::Value::String(fresh_seed().to_string()));
        }
    } else {
        config.remove("rotation");
    }
    let text = match toml::to_string(&config) {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    if let Some(dir) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            return fail(format!("{}: {e}", dir.display()));
        }
    }
    if let Err(e) = std::fs::write(&path, text) {
        return fail(format!("{}: {e}", path.display()));
    }
    let rotation = config.get("rotation").and_then(toml::Value::as_str).unwrap_or("-");
    println!(
        "korvus setup: wrote {}: identity = {}, rotation = {rotation}{}",
        path.display(),
        if stealth { "stealth" } else { "korvus" },
        if asked { "" } else { " (no terminal: the defaults, unasked)" }
    );
    0
}

// ── personas ─────────────────────────────────────────────────────────────────────────────────────

fn personas_dir() -> Option<PathBuf> {
    Some(settings::home()?.join("personas"))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && name.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

fn read_doc(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    persona::document::read(&text).map_err(|r| format!("{}: {}", path.display(), r.join("; ")))?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

fn write_doc(path: &Path, doc: &Value) -> Result<(), String> {
    let text = serde_json::to_string_pretty(doc).map_err(|e| e.to_string())?;
    persona::document::read(&text).map_err(|r| format!("the persona would not read: {}", r.join("; ")))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))
}

pub fn persona(args: &[String]) -> i32 {
    let Some(root) = personas_dir() else { return fail("no home directory for personas") };
    match args.first().map(String::as_str) {
        Some("new") => {
            let Some(name) = args.get(1).filter(|n| valid_name(n)) else { return fail("usage: korvus persona new <name> [--from <seed:<os>:<x> | document.json>] (letters, digits, `.`, `_`, `-`)") };
            let dir = root.join(name);
            if dir.join("persona.json").exists() {
                return fail(format!("persona {name} exists ({})", dir.display()));
            }
            let from = args.iter().position(|a| a == "--from").and_then(|i| args.get(i + 1)).cloned();
            let doc = match from.as_deref() {
                Some(path) if path.ends_with(".json") => read_doc(Path::new(path)).map(|mut d| {
                    d["name"] = json!(name);
                    d
                }),
                selector => draw(name, selector),
            };
            match doc.and_then(|d| write_doc(&dir.join("persona.json"), &d).map(|_| d)) {
                Ok(d) => {
                    println!("korvus persona new: {name} -- {}", summary(&d));
                    0
                }
                Err(e) => fail(e),
            }
        }
        Some("set") => {
            let Some(name) = args.get(1).filter(|n| valid_name(n)) else { return fail("usage: korvus persona set <name> <key>=<value>...") };
            let path = root.join(name).join("persona.json");
            let mut doc = match read_doc(&path) {
                Ok(d) => d,
                Err(e) => return fail(e),
            };
            for kv in &args[2..] {
                let Some((k, v)) = kv.split_once('=') else { return fail(format!("{kv}: not key=value")) };
                let value = match k {
                    "proxy" | "timezone" | "locale" => json!(v),
                    "humanize" => json!(matches!(v, "1" | "true" | "yes" | "on")),
                    _ => return fail(format!("{k}: a persona sets proxy, timezone, locale or humanize")),
                };
                if !doc["session"].is_object() {
                    doc["session"] = json!({});
                }
                if v.is_empty() {
                    if let Some(s) = doc["session"].as_object_mut() {
                        s.remove(k);
                    }
                } else {
                    doc["session"][k] = value;
                }
            }
            match write_doc(&path, &doc) {
                Ok(()) => {
                    println!("korvus persona set: {name} -- {}", summary(&doc));
                    0
                }
                Err(e) => fail(e),
            }
        }
        Some("ls") => {
            let mut names: Vec<String> = std::fs::read_dir(&root)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| e.path().join("persona.json").is_file())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            if names.is_empty() {
                println!("no persona ({})", root.display());
            }
            for n in names {
                let dir = root.join(&n);
                let doc = read_doc(&dir.join("persona.json"));
                let kept = dir.join("profile").join("store.bin").is_file();
                match doc {
                    Ok(d) => println!("{n}\t{}\t{}", summary(&d), if kept { "profile kept" } else { "no profile yet" }),
                    Err(e) => println!("{n}\t(does not read: {e})"),
                }
            }
            0
        }
        Some("rm") => {
            let Some(name) = args.get(1).filter(|n| valid_name(n)) else { return fail("usage: korvus persona rm <name>") };
            let dir = root.join(name);
            if !dir.join("persona.json").is_file() {
                return fail(format!("no persona {name}"));
            }
            match std::fs::remove_dir_all(&dir) {
                Ok(()) => {
                    println!("korvus persona rm: {name} (its document and its profile)");
                    0
                }
                Err(e) => fail(format!("{}: {e}", dir.display())),
            }
        }
        // A clean start for a persona that remembers: its profile (cookies, storage) dropped, its document kept.
        Some("reset") => {
            let Some(name) = args.get(1).filter(|n| valid_name(n)) else { return fail("usage: korvus persona reset <name>") };
            let dir = root.join(name);
            if !dir.join("persona.json").is_file() {
                return fail(format!("no persona {name}"));
            }
            match std::fs::remove_dir_all(dir.join("profile")) {
                Ok(()) => println!("korvus persona reset: {name} (its profile; the next context starts clean)"),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => println!("korvus persona reset: {name} had no profile"),
                Err(e) => return fail(format!("{}: {e}", dir.display())),
            }
            0
        }
        // A persona's state as a file: every slot of its profile (cookies, each origin's storage, IndexedDB, caches),
        // and its cookies as DevTools reads them, for reading. `import` puts a file back as the profile.
        Some("export") => {
            let Some(name) = args.get(1).filter(|n| valid_name(n)) else { return fail("usage: korvus persona export <name> [file]") };
            let store = root.join(name).join("profile").join("store.bin");
            let state = match profile_json(&store) {
                Ok(v) => v,
                Err(e) => return fail(format!("{}: {e}", store.display())),
            };
            let text = serde_json::to_string_pretty(&state).unwrap_or_default();
            match args.get(2) {
                Some(out) => match std::fs::write(out, text + "\n") {
                    Ok(()) => {
                        eprintln!("korvus persona export: {name} -> {out}");
                        0
                    }
                    Err(e) => fail(format!("{out}: {e}")),
                },
                None => {
                    println!("{text}");
                    0
                }
            }
        }
        Some("import") => {
            let (Some(name), Some(file)) = (args.get(1).filter(|n| valid_name(n)), args.get(2)) else { return fail("usage: korvus persona import <name> <file>") };
            let dir = root.join(name);
            if !dir.join("persona.json").is_file() {
                return fail(format!("no persona {name}"));
            }
            let state: Value = match std::fs::read_to_string(file).map_err(|e| e.to_string()).and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string())) {
                Ok(v) => v,
                Err(e) => return fail(format!("{file}: {e}")),
            };
            match write_profile(&state, &dir.join("profile").join("store.bin")) {
                Ok(n) => {
                    println!("korvus persona import: {name} <- {file} ({n} slots)");
                    0
                }
                Err(e) => fail(format!("{file}: {e}")),
            }
        }
        _ => fail("usage: korvus persona new|set|ls|rm|reset|export|import ..."),
    }
}

/// A profile (`store.bin`: the context store's slots, `Server::save`'s format) as JSON: `{"korvus_profile": 1,
/// "slots": {name: base64}, "cookies": [...]}`, the cookies as `Storage.getCookies` reports them (HttpOnly ones
/// included). A persona with no profile yet exports no slots.
fn profile_json(store: &Path) -> Result<Value, String> {
    use base64::Engine as _;
    let rows: Vec<(String, Vec<u8>)> = match std::fs::read(store) {
        Ok(b) => bincode::deserialize(&b).map_err(|e| e.to_string())?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.to_string()),
    };
    let cookies: Vec<Value> = rows
        .iter()
        .find(|(k, _)| k == "cookies")
        .map(|(_, jar)| {
            transport::cookies::devtools_cookies_in(jar)
                .iter()
                .map(|c| json!({ "name": c.name, "value": c.value, "domain": c.domain, "path": c.path, "expires": c.expires, "httpOnly": c.http_only, "secure": c.secure, "sameSite": c.same_site }))
                .collect()
        })
        .unwrap_or_default();
    let slots: serde_json::Map<String, Value> = rows.into_iter().map(|(k, v)| (k, json!(base64::engine::general_purpose::STANDARD.encode(v)))).collect();
    Ok(json!({ "korvus_profile": 1, "slots": slots, "cookies": cookies }))
}

/// [`profile_json`]'s file written back as a profile; how many slots it held.
fn write_profile(state: &Value, store: &Path) -> Result<usize, String> {
    use base64::Engine as _;
    let slots = state.get("slots").and_then(Value::as_object).ok_or("not a korvus profile export (no `slots`)")?;
    let mut rows: Vec<(String, Vec<u8>)> = Vec::with_capacity(slots.len());
    for (k, v) in slots {
        let b64 = v.as_str().ok_or_else(|| format!("slot {k} is not base64 text"))?;
        rows.push((k.clone(), base64::engine::general_purpose::STANDARD.decode(b64).map_err(|e| format!("slot {k}: {e}"))?));
    }
    rows.sort();
    let body = bincode::serialize(&rows).map_err(|e| e.to_string())?;
    if let Some(dir) = store.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(store, body).map_err(|e| e.to_string())?;
    Ok(rows.len())
}

/// A persona's device in a few words: its OS, cores and memory, its GPU, its session.
fn summary(doc: &Value) -> String {
    let d = &doc["device"];
    let mut s = d["os"].as_str().unwrap_or("?").to_string();
    if let Some(c) = d["hardware"]["cores"].as_u64() {
        s += &format!(", {c} cores, {} GB", d["hardware"]["memory_gb"]);
    }
    if let Some(r) = d["gpu"]["renderer"].as_str() {
        s += &format!(", {r}");
    }
    if let Some(sess) = doc["session"].as_object().filter(|o| !o.is_empty()) {
        s += &format!(", session {}", Value::Object(sess.clone()));
    }
    s
}

/// A pool device for `name`, drawn by the stealth identity's device slot (the pool is korvus-stealth's, goal 1020
/// invariant I5): `selector` is `seed:<os>:<x>`, its OS this host's persona OS; none is `seed:<os>:<name>`.
fn draw(name: &str, selector: Option<&str>) -> Result<Value, String> {
    let os = if persona::host_os() == "macos" { "macos" } else { "windows" };
    let key = match selector {
        None => name.to_string(),
        Some(s) => {
            let mut parts = s.splitn(3, ':');
            match (parts.next(), parts.next(), parts.next()) {
                (Some("seed"), Some(o), Some(x)) if o == os && !x.is_empty() => x.to_string(),
                (Some("seed"), Some(o), Some(_)) => return Err(format!("this host presents {os} personas, not {o} (one persona OS per host)")),
                _ => return Err(format!("--from {s}: a pool selector (seed:{os}:<anything>) or a persona document (.json)")),
            }
        }
    };
    let lib = crate::stealth_library()?;
    let (path, config) = plugin_host::split_arg(&lib);
    plugin_host::load(Path::new(&path), &config).map_err(|e| format!("korvus-stealth: {e}"))?;
    let info = plugin_host::ContextInfo {
        context_id: format!("persona-{name}"),
        name: name.to_string(),
        host_os: persona::host_os().to_string(),
        identity: "stealth".into(),
        config_json: json!({ "name": name, "seed": key, "device": "pool" }).to_string(),
        ..Default::default()
    };
    let seed = plugin_host::rotation(&info).map(|r| r.0).unwrap_or(0);
    let device = plugin_host::device(&info, seed).ok_or("korvus-stealth drew no device")?;
    let device: Value = serde_json::from_str(&device).map_err(|e| e.to_string())?;
    Ok(json!({ "persona": 1, "name": name, "device": device, "build": { "browser": "chrome", "os": device["os"] } }))
}

// ── plugin ls, plugin check, stats ─────────────────────────────────────────────────────────────────

/// The plugins `settings` load (already loaded by the caller), one a line: name, version, slots, path.
pub fn plugin_ls(s: &Settings) -> i32 {
    let loaded = plugin_host::loaded();
    if loaded.is_empty() {
        println!("no plugin (identity {}: korvus's own)", s.identity);
    }
    for p in loaded.iter() {
        println!("{} {}\t{}\t{}", p.name, p.version, p.slots().join(", "), p.path.display());
    }
    0
}

/// `korvus plugin check <path>[:<config JSON>]` — load one plugin library in isolation (no identity, no run) and
/// print what its descriptor declares: name, version, ABI, size, slots, labels and CDP domains. Exits 0 when it
/// loaded, else non-zero -- `plugin_host::load` has already said the refusal's reason on stderr.
pub fn plugin_check(args: &[String]) -> i32 {
    let Some(arg) = args.first() else { return fail("usage: korvus plugin check <path>[:<config JSON>]") };
    let (path, config) = plugin_host::split_arg(arg);
    match plugin_host::load(Path::new(&path), &config) {
        Ok(p) => {
            println!("name\t{}", p.name);
            println!("version\t{}", p.version);
            println!("abi\t{}", p.abi());
            println!("size\t{} bytes", p.size());
            println!("slots\t{}", if p.slots().is_empty() { "(none)".into() } else { p.slots().join(", ") });
            println!("labels\t{}", if p.labels.is_empty() { "(none)".into() } else { p.labels.join(", ") });
            println!("cdp\t{}", if p.cdp_domains.is_empty() { "(none)".into() } else { p.cdp_domains.join(", ") });
            0
        }
        // `load` already printed `[korvus] plugin: refused <path>: <why>`; a non-zero exit is all that is left.
        Err(_) => 2,
    }
}

pub fn stats(args: &[String]) -> i32 {
    let port: u16 = args.iter().position(|a| a == "--port").and_then(|i| args.get(i + 1)).and_then(|p| p.parse().ok()).unwrap_or(9222);
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut s = match std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(3)) {
        Ok(s) => s,
        Err(e) => return fail(format!("no korvus serve on {addr}: {e}")),
    };
    let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(5)));
    let _ = write!(s, "GET /korvus/sessions HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    let mut raw = String::new();
    let _ = s.read_to_string(&mut raw);
    let body = raw.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
    let Ok(v) = serde_json::from_str::<Value>(body.trim()) else { return fail(format!("{addr} answered no sessions: {}", body.trim())) };
    println!("active {}", v["active"]);
    for c in v["contexts"].as_array().into_iter().flatten() {
        println!("  {}\t{}\t{} page(s)", c["context"].as_str().unwrap_or(""), c["name"].as_str().unwrap_or("-"), c["pages"]);
    }
    0
}
