//! The settings a run takes (goal 1020 step 5, the design's §8): each from the first of its flag, its environment
//! variable (`KORVUS_<NAME>`), `config.toml` (`$KORVUS_CONFIG`, else `~/.korvus/config.toml`) and the built-in
//! default. A context's own parameters -- its connect URL, its `createBrowserContext` parameters -- go over these for
//! that context (`cdp::router::Defaults`).
//!
//! The built-in defaults are korvus as it ran before this file: the stealth identity, no rotation (the build's own
//! device, read back raw), no humanize. `korvus setup` writes the defaults its two questions choose.

use std::path::PathBuf;

/// The settings, resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// `stealth` (the korvus-stealth plugin) or `korvus` (korvus's own, no identity plugin).
    pub identity: String,
    /// `rotate` (a fresh persona per session), `fixed` (one, `seed`), `named` (the persona's name is its seed) or
    /// `off` (the build's own device).
    pub rotation: String,
    pub seed: Option<String>,
    /// `pool`, `host` or a persona document's path.
    pub device: Option<String>,
    /// `http://user:pass@host:port`, `socks5://user:pass@host:port`.
    pub proxy: Option<String>,
    pub timezone: Option<String>,
    pub locale: Option<String>,
    pub humanize: bool,
    /// korvus-stealth's readback noise for a seeded persona.
    pub noise: bool,
    /// When the worn persona describes a machine other than this host (`ignore` warns and runs, `strict` refuses):
    /// the stealth identity presents a device unlike the host by design, so it defaults to `ignore`; korvus's own
    /// identity wears this machine, so it defaults to `strict`. The engine honours it through `KORVUS_HOST`.
    pub host: String,
    /// `[[plugin]]` entries, then `--plugin` arguments, as `path[:config JSON]`.
    pub plugins: Vec<String>,
}

pub const IDENTITIES: &[&str] = &["stealth", "korvus"];
pub const ROTATIONS: &[&str] = &["rotate", "fixed", "named", "off"];
pub const HOSTS: &[&str] = &["ignore", "strict"];

/// The config file.
pub fn config_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("KORVUS_CONFIG").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    Some(home()?.join("config.toml"))
}

/// korvus's own directory: `$KORVUS_HOME`, else `~/.korvus`.
pub fn home() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("KORVUS_HOME").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let h = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })?;
    Some(PathBuf::from(h).join(".korvus"))
}

/// The config file as a TOML table (an empty one when there is none). A file that does not parse is the run's end.
pub fn read_config() -> Result<toml::Table, String> {
    let Some(path) = config_path() else { return Ok(toml::Table::new()) };
    let Ok(text) = std::fs::read_to_string(&path) else { return Ok(toml::Table::new()) };
    toml::from_str(&text).map_err(|e| format!("{} is not valid TOML: {e}", path.display()))
}

/// The flags this file resolves, taken out of `argv`: `--identity`, `--rotate`, `--seed` (`--persona-seed`, its
/// older name), `--device`, `--proxy`, `--timezone`, `--locale`, `--humanize` / `--no-humanize`, `--noise` /
/// `--no-noise`, and every `--plugin`.
#[derive(Debug, Default, Clone)]
pub struct Flags {
    pub identity: Option<String>,
    pub rotation: Option<String>,
    pub seed: Option<String>,
    pub device: Option<String>,
    pub proxy: Option<String>,
    pub timezone: Option<String>,
    pub locale: Option<String>,
    pub humanize: Option<bool>,
    pub noise: Option<bool>,
    pub host: Option<String>,
    pub plugins: Vec<String>,
}

impl Flags {
    pub fn take(argv: &mut Vec<String>) -> Result<Flags, String> {
        let mut f = Flags::default();
        let value = |argv: &mut Vec<String>, names: &[&str]| -> Result<Option<String>, String> {
            let mut got = None;
            while let Some(i) = argv.iter().position(|a| names.contains(&a.as_str())) {
                let v = argv.get(i + 1).cloned().ok_or_else(|| format!("{} needs a value", argv[i]))?;
                argv.drain(i..i + 2);
                got = Some(v);
            }
            Ok(got)
        };
        f.identity = value(argv, &["--identity"])?;
        f.rotation = value(argv, &["--rotate"])?;
        f.seed = value(argv, &["--seed", "--persona-seed"])?;
        f.device = value(argv, &["--device"])?;
        f.proxy = value(argv, &["--proxy"])?;
        f.timezone = value(argv, &["--timezone"])?;
        f.locale = value(argv, &["--locale"])?;
        f.host = value(argv, &["--host"])?;
        let switch = |argv: &mut Vec<String>, on: &str, off: &str| -> Option<bool> {
            let mut got = None;
            while let Some(i) = argv.iter().position(|a| a == on || a == off) {
                got = Some(argv[i] == on);
                argv.remove(i);
            }
            got
        };
        f.humanize = switch(argv, "--humanize", "--no-humanize");
        f.noise = switch(argv, "--noise", "--no-noise");
        while let Some(i) = argv.iter().position(|a| a == "--plugin") {
            let v = argv.get(i + 1).cloned().ok_or("--plugin needs a library path")?;
            argv.drain(i..i + 2);
            f.plugins.push(v);
        }
        Ok(f)
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn env_bool(name: &str) -> Result<Option<bool>, String> {
    match env(name).as_deref() {
        None => Ok(None),
        Some("1" | "true" | "yes" | "on") => Ok(Some(true)),
        Some("0" | "false" | "no" | "off") => Ok(Some(false)),
        Some(other) => Err(format!("{name}={other:?} is not true or false")),
    }
}

fn text(t: &toml::Table, key: &str) -> Option<String> {
    match t.get(key)? {
        toml::Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        toml::Value::Integer(n) => Some(n.to_string()),
        _ => None,
    }
}

/// `[[plugin]]` entries: `path`, and an optional `config` table handed to the plugin as JSON.
pub fn config_plugins(t: &toml::Table) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for p in t.get("plugin").and_then(toml::Value::as_array).into_iter().flatten() {
        let lib = p.get("path").and_then(toml::Value::as_str).ok_or("a [[plugin]] in the config file has no path")?;
        let config = p.get("config").map(|c| serde_json::to_string(c).unwrap_or_else(|_| "{}".into())).unwrap_or_else(|| "{}".into());
        out.push(format!("{lib}:{config}"));
    }
    Ok(out)
}

/// The host posture a given identity defaults to when none is set (see `Settings::host`).
fn default_host(identity: &str) -> &'static str {
    if identity == "stealth" { "ignore" } else { "strict" }
}

impl Settings {
    /// Each setting from the first of `flags`, the environment and `config`, else its built-in default.
    pub fn resolve(flags: &Flags, config: &toml::Table) -> Result<Settings, String> {
        let pick = |flag: &Option<String>, name: &str, key: &str| flag.clone().or_else(|| env(name)).or_else(|| text(config, key));
        let boolean = |flag: Option<bool>, name: &str, key: &str| -> Result<Option<bool>, String> {
            Ok(match flag {
                Some(b) => Some(b),
                None => match env_bool(name)? {
                    Some(b) => Some(b),
                    None => match config.get(key) {
                        None => None,
                        Some(toml::Value::Boolean(b)) => Some(*b),
                        Some(other) => return Err(format!("config `{key}` is {other}, not true or false")),
                    },
                },
            })
        };
        let identity = pick(&flags.identity, "KORVUS_IDENTITY", "identity").unwrap_or_else(|| "stealth".into());
        // The host posture follows the identity unless it is set outright: stealth presents a device unlike this
        // host on purpose, so it runs (`ignore`); korvus's own identity wears this machine, so a mismatch is an
        // accident worth refusing (`strict`).
        let host = pick(&flags.host, "KORVUS_HOST", "host").unwrap_or_else(|| default_host(&identity).into());
        let s = Settings {
            identity,
            rotation: pick(&flags.rotation, "KORVUS_ROTATION", "rotation").unwrap_or_else(|| "off".into()),
            seed: pick(&flags.seed, "KORVUS_SEED", "seed"),
            device: pick(&flags.device, "KORVUS_DEVICE", "device"),
            proxy: pick(&flags.proxy, "KORVUS_PROXY", "proxy"),
            timezone: pick(&flags.timezone, "KORVUS_TIMEZONE", "timezone"),
            locale: pick(&flags.locale, "KORVUS_LOCALE", "locale"),
            humanize: boolean(flags.humanize, "KORVUS_HUMANIZE", "humanize")?.unwrap_or(false),
            noise: boolean(flags.noise, "KORVUS_NOISE", "noise")?.unwrap_or(true),
            host,
            plugins: config_plugins(config)?.into_iter().chain(flags.plugins.iter().cloned()).collect(),
        };
        if !IDENTITIES.contains(&s.identity.as_str()) {
            return Err(format!("identity is stealth or korvus, not {:?}", s.identity));
        }
        if !HOSTS.contains(&s.host.as_str()) {
            return Err(format!("host is ignore or strict, not {:?} (it is also KORVUS_HOST, which the engine reads)", s.host));
        }
        if !ROTATIONS.contains(&s.rotation.as_str()) {
            return Err(format!("rotation is rotate, fixed, named or off, not {:?}", s.rotation));
        }
        if s.rotation == "fixed" && s.seed.is_none() {
            return Err("rotation = fixed needs a seed (`--seed`, `seed` in the config; `korvus setup` draws one)".into());
        }
        if s.identity == "korvus" && s.device.as_deref().is_some_and(|d| d != "host") {
            return Err("korvus's own identity wears this machine; a device needs the stealth identity".into());
        }
        Ok(s)
    }

    /// korvus-stealth's own configuration for these settings (`rotation`, `noise`), as the JSON after its path.
    pub fn stealth_config(&self) -> String {
        let rotation = match self.rotation.as_str() {
            "rotate" => "rotate",
            "named" => "named",
            _ => "off",
        };
        serde_json::json!({ "rotation": rotation, "noise": self.noise }).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(text: &str) -> toml::Table {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn a_flag_wins_over_the_config_and_the_config_over_the_default() {
        let c = config("identity = \"korvus\"\nrotation = \"rotate\"\ntimezone = \"Europe/Paris\"\nhumanize = true\n");
        let s = Settings::resolve(&Flags::default(), &c).unwrap();
        assert_eq!((s.identity.as_str(), s.rotation.as_str(), s.timezone.as_deref(), s.humanize), ("korvus", "rotate", Some("Europe/Paris"), true));
        let mut argv: Vec<String> = ["korvus", "serve", "--identity", "stealth", "--timezone", "Asia/Tokyo", "--no-humanize"].map(String::from).to_vec();
        let f = Flags::take(&mut argv).unwrap();
        assert_eq!(argv, ["korvus", "serve"]);
        let s = Settings::resolve(&f, &c).unwrap();
        assert_eq!((s.identity.as_str(), s.rotation.as_str(), s.timezone.as_deref(), s.humanize), ("stealth", "rotate", Some("Asia/Tokyo"), false));
        let d = Settings::resolve(&Flags::default(), &toml::Table::new()).unwrap();
        assert_eq!((d.identity.as_str(), d.rotation.as_str(), d.humanize, d.noise), ("stealth", "off", false, true));
    }

    #[test]
    fn the_config_s_plugins_come_before_the_flags() {
        let c = config("[[plugin]]\npath = \"/a.so\"\nconfig = { x = 1 }\n");
        let mut argv: Vec<String> = ["korvus", "--plugin", "/b.so"].map(String::from).to_vec();
        let s = Settings::resolve(&Flags::take(&mut argv).unwrap(), &c).unwrap();
        assert_eq!(s.plugins, ["/a.so:{\"x\":1}", "/b.so"]);
    }

    #[test]
    fn the_host_posture_follows_the_identity_and_the_flag_wins() {
        // the default follows the identity (pure, so no ambient KORVUS_HOST can sway it)
        assert_eq!(default_host("stealth"), "ignore");
        assert_eq!(default_host("korvus"), "strict");
        // `--host` wins over the default and over the ambient KORVUS_HOST the gate runs under
        let mut argv: Vec<String> = ["korvus", "serve", "--host", "strict"].map(String::from).to_vec();
        let f = Flags::take(&mut argv).unwrap();
        assert_eq!(argv, ["korvus", "serve"]);
        assert_eq!(Settings::resolve(&f, &toml::Table::new()).unwrap().host, "strict");
        // and a value that is neither is refused
        let mut bad: Vec<String> = ["korvus", "--host", "loose"].map(String::from).to_vec();
        assert!(Settings::resolve(&Flags::take(&mut bad).unwrap(), &toml::Table::new()).is_err());
    }

    #[test]
    fn a_setting_that_cannot_hold_is_refused() {
        assert!(Settings::resolve(&Flags::default(), &config("identity = \"chrome\"")).is_err());
        assert!(Settings::resolve(&Flags::default(), &config("rotation = \"fixed\"")).is_err());
        assert!(Settings::resolve(&Flags::default(), &config("identity = \"korvus\"\ndevice = \"pool\"")).is_err());
        assert!(Settings::resolve(&Flags::default(), &config("humanize = \"yes\"")).is_err());
    }
}
