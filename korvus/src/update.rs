//! Updates (D183): `korvus update` installs the latest release as this korvus was installed, and a korvus command run
//! in a terminal checks for one, at most once a day, and asks to update as Oh My Zsh does:
//!
//!   korvus 152.0.4 is out (you have 152.0.3). Update now? [Y/n]
//!
//! Oh My Zsh's modes are korvus's: `prompt` (the default) asks, `auto` updates without asking, `reminder` only says a
//! release is out, `off` never checks. `config.toml`'s `[update]` sets them (`mode`, and `frequency` in days, 1 by
//! default); `KORVUS_UPDATE=prompt|auto|reminder|off` overrides it for a run. Nothing is checked, asked or installed
//! outside a terminal (stdin and stderr both one, and no `CI`), so a script, a CI job or a server is never held up.
//!
//! The check is one request to GitHub's API for the latest release's tag; it sends nothing about the machine or its
//! pages, and its answer is kept in `<korvus home>/update-check` (the time and the tag) until the next is due.
//!
//! How an update installs depends on how this korvus came: the installer's (`~/.korvus/dist/<version>`, `current`
//! pointing at it) runs that release's own installer for its tag, which unpacks it beside this one and re-points
//! `current`, so a `korvus serve` already running keeps its files; pip's launcher (it names its Python in
//! `KORVUS_PIP_PYTHON`) runs `pip install --upgrade korvus-browser`; anything else (an archive unpacked by hand, a
//! build) is told how instead.

use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::{settings, VERSION};

const LATEST: &str = "https://api.github.com/repos/KorvusBrowser/korvus-browser/releases/latest";
const RELEASES: &str = "https://github.com/KorvusBrowser/korvus-browser/releases";
/// How long the check may take: a slow network never holds a command up for longer.
const WITHIN: Duration = Duration::from_secs(4);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    Prompt,
    Auto,
    Reminder,
    Off,
}

impl Mode {
    fn parse(s: &str) -> Option<Mode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "prompt" => Some(Mode::Prompt),
            "auto" => Some(Mode::Auto),
            "reminder" => Some(Mode::Reminder),
            "off" | "disabled" => Some(Mode::Off),
            _ => None,
        }
    }
}

/// `KORVUS_UPDATE`, else `config.toml`'s `[update] mode`, else `prompt`.
pub fn mode() -> Mode {
    if let Some(m) = std::env::var("KORVUS_UPDATE").ok().as_deref().and_then(Mode::parse) {
        return m;
    }
    let config = settings::read_config().unwrap_or_default();
    config.get("update").and_then(|u| u.get("mode")).and_then(|m| m.as_str()).and_then(Mode::parse).unwrap_or(Mode::Prompt)
}

/// Seconds between checks: `[update] frequency` in days (1 by default; 0 checks every time).
fn frequency() -> u64 {
    let config = settings::read_config().unwrap_or_default();
    let days = config.get("update").and_then(|u| u.get("frequency")).and_then(|f| f.as_integer().or_else(|| f.as_float().map(|d| d as i64)));
    days.unwrap_or(1).max(0) as u64 * 86_400
}

/// Whether `a` (dotted numbers, `152.0.4`) is a later release than `b`.
pub fn newer(a: &str, b: &str) -> bool {
    let parts = |s: &str| s.trim_start_matches('v').split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    parts(a) > parts(b)
}

/// The latest release's version (`152.0.4`), from GitHub's API.
fn latest() -> Result<String, String> {
    let agent = format!("korvus/{VERSION}");
    let headers = [("accept", "application/vnd.github+json"), ("user-agent", agent.as_str())];
    let (status, body) = transport::http_request("GET", LATEST, &headers, None, None, WITHIN)?;
    if status != 200 {
        return Err(format!("GitHub answered {status}"));
    }
    let json: serde_json::Value = serde_json::from_slice(&body).map_err(|_| "GitHub's answer is not JSON".to_string())?;
    let tag = json["tag_name"].as_str().ok_or("GitHub named no release")?;
    Ok(tag.trim_start_matches('v').to_string())
}

/// How this korvus was installed, which says how to update it.
enum Install {
    /// The installer's: `<home>/dist/<version>/korvus`, with `<home>/current` pointing at the newest.
    Installer { home: PathBuf },
    /// pip's launcher, which names its Python.
    Pip { python: String },
    /// Unpacked by hand or built: no update of korvus's own to run.
    Other,
}

fn install() -> Install {
    if let Some(python) = std::env::var("KORVUS_PIP_PYTHON").ok().filter(|p| !p.is_empty()) {
        return Install::Pip { python };
    }
    let (Ok(exe), Some(home)) = (std::env::current_exe().and_then(|e| e.canonicalize()), settings::home()) else { return Install::Other };
    match (home.join("dist").canonicalize(), exe.parent().and_then(|v| v.parent())) {
        (Ok(dist), Some(parent)) if parent == dist => Install::Installer { home },
        _ => Install::Other,
    }
}

/// Install release `version` as this korvus was installed. `Ok(true)`: installed, and `<home>/current` is it.
fn install_release(version: &str) -> Result<bool, String> {
    match install() {
        Install::Installer { home } => {
            let tag = format!("v{version}");
            let mut cmd = if cfg!(windows) {
                let script = format!("irm '{RELEASES}/download/{tag}/install.ps1' | iex");
                let mut c = Command::new("powershell");
                c.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &script]);
                c
            } else {
                // The release's own installer for its tag, fetched first so a failed download is an error, not an
                // empty script.
                let script = r#"t=$(mktemp) || exit 1
if command -v curl >/dev/null 2>&1; then curl -fsSL "$1" -o "$t"; else wget -qO "$t" "$1"; fi && sh "$t"
s=$?; rm -f "$t"; exit $s"#;
                let mut c = Command::new("sh");
                c.args(["-c", script, "sh", &format!("{RELEASES}/download/{tag}/install.sh")]);
                c
            };
            // The same home, the user's settings kept, the PATH left as the first install set it, no questions.
            cmd.env("KORVUS_HOME", &home).env("KORVUS_VERSION", &tag).env("KORVUS_NO_MODIFY_PATH", "1").env("KORVUS_NO_PROMPT", "1");
            let status = cmd.status().map_err(|e| format!("could not run the installer: {e}"))?;
            if !status.success() {
                return Err(format!("the installer of {tag} failed ({status})"));
            }
            Ok(true)
        }
        Install::Pip { python } => {
            let status = Command::new(&python)
                .args(["-m", "pip", "install", "--upgrade", "korvus-browser"])
                .status()
                .map_err(|e| format!("could not run pip ({python}): {e}"))?;
            if !status.success() {
                return Err(format!("pip install --upgrade korvus-browser failed ({status})"));
            }
            Ok(false)
        }
        Install::Other => Err(format!(
            "this korvus was not installed by korvus's installer or pip, so it cannot update itself: install with \
             `curl -fsSL https://korvusbrowser.com/install.sh | bash` (Windows: `irm https://korvusbrowser.com/install.ps1 | iex`), \
             or download {version} from {RELEASES}"
        )),
    }
}

/// `korvus update [--check]`: install the latest release if it is newer; `--check` only says whether there is one.
pub fn update_command(args: &[String]) -> i32 {
    let check_only = args.iter().any(|a| a == "--check");
    let latest = match latest() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("korvus: cannot reach GitHub for the latest release: {e}");
            return 2;
        }
    };
    remember(&latest);
    if !newer(&latest, VERSION) {
        println!("korvus {VERSION} is the latest");
        return 0;
    }
    if check_only {
        println!("korvus {latest} is out (you have {VERSION}): `korvus update` installs it");
        return 0;
    }
    if !matches!(install(), Install::Other) {
        eprintln!("korvus: updating {VERSION} -> {latest}");
    }
    match install_release(&latest) {
        Ok(true) => {
            println!("korvus {latest} is installed; a korvus serve already running keeps {VERSION} until it restarts");
            0
        }
        Ok(false) => {
            println!("korvus-browser is upgraded: the next `korvus` runs {latest}");
            0
        }
        Err(e) => {
            eprintln!("korvus: {e}");
            1
        }
    }
}

fn state_path() -> Option<PathBuf> {
    Some(settings::home()?.join("update-check"))
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default()
}

/// When the last check ran, from `<home>/update-check` ("<unix time> <version>").
fn last_check() -> Option<u64> {
    let text = std::fs::read_to_string(state_path()?).ok()?;
    text.split_whitespace().next()?.parse().ok()
}

fn remember(latest: &str) {
    if let Some(path) = state_path() {
        let _ = path.parent().map(std::fs::create_dir_all);
        let _ = std::fs::write(path, format!("{} {latest}\n", now()));
    }
}

/// The check a command run in a terminal makes before it starts (`serve`, `persona`, `stats`, ...). Returns `true`
/// when it installed a newer korvus that the caller should run in its place.
pub fn on_start() -> bool {
    let mode = mode();
    if mode == Mode::Off || std::env::var_os("CI").is_some() || !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
        return false;
    }
    if last_check().is_some_and(|at| now().saturating_sub(at) < frequency()) {
        return false;
    }
    let Ok(latest) = latest() else { return false };
    remember(&latest);
    if !newer(&latest, VERSION) {
        return false;
    }
    let off = "KORVUS_UPDATE=off, or `[update] mode = \"off\"` in config.toml, stops these checks";
    match mode {
        Mode::Reminder => {
            eprintln!("korvus {latest} is out (you have {VERSION}): `korvus update` installs it ({off})");
            return false;
        }
        Mode::Prompt => {
            eprint!("korvus {latest} is out (you have {VERSION}). Update now? [Y/n] ");
            let _ = std::io::stderr().flush();
            let mut answer = String::new();
            let _ = std::io::stdin().lock().read_line(&mut answer);
            if !matches!(answer.trim().to_ascii_lowercase().as_str(), "" | "y" | "yes") {
                eprintln!("Not now: `korvus update` installs it any time; {off}.");
                return false;
            }
        }
        Mode::Auto => eprintln!("korvus {latest} is out (you have {VERSION}): updating"),
        Mode::Off => return false,
    }
    match install_release(&latest) {
        Ok(true) => true,
        Ok(false) => {
            eprintln!("korvus-browser is upgraded: the next `korvus` runs {latest}");
            false
        }
        Err(e) => {
            eprintln!("korvus: the update did not install ({e}); carrying on with {VERSION}");
            false
        }
    }
}

/// Run the korvus `<home>/current` now points at, with this run's arguments, in this one's place: the update the
/// check just installed starts at once.
pub fn run_current(args: &[std::ffi::OsString]) -> ! {
    let Some(exe) = settings::home().map(|h| h.join("current").join(if cfg!(windows) { "korvus.exe" } else { "korvus" })) else {
        std::process::exit(1)
    };
    let mut cmd = Command::new(&exe);
    cmd.args(args.iter().skip(1));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = cmd.exec();
        eprintln!("korvus: could not start {}: {err}", exe.display());
        std::process::exit(1)
    }
    #[cfg(not(unix))]
    {
        match cmd.status() {
            Ok(s) => std::process::exit(s.code().unwrap_or(1)),
            Err(err) => {
                eprintln!("korvus: could not start {}: {err}", exe.display());
                std::process::exit(1)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_later_release_is_newer() {
        assert!(newer("152.0.4", "152.0.3"));
        assert!(newer("v153.0.0", "152.9.9"));
        assert!(newer("152.0.10", "152.0.9"));
        assert!(!newer("152.0.3", "152.0.3"));
        assert!(!newer("152.0.2", "152.0.3"));
    }

    #[test]
    fn the_modes_are_oh_my_zsh_s() {
        assert_eq!(Mode::parse("prompt"), Some(Mode::Prompt));
        assert_eq!(Mode::parse("Auto"), Some(Mode::Auto));
        assert_eq!(Mode::parse("reminder"), Some(Mode::Reminder));
        assert_eq!(Mode::parse("off"), Some(Mode::Off));
        assert_eq!(Mode::parse("disabled"), Some(Mode::Off));
        assert_eq!(Mode::parse("sometimes"), None);
    }
}
