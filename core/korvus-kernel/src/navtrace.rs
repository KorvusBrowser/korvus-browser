//! WHERE A NAVIGATION'S TIME GOES, phase by phase. Off unless `KORVUS_NAV_TRACE`
//! names a file to append to; when it is off every call costs one `OnceLock` read.
//!
//! It exists because `nav_to_load_ms` is a single number and the things inside it -- building the
//! realm the document loads in, fetching the document, unloading the one being left, parsing and
//! running the page's scripts, waiting out the load-delay set -- move independently. A ranking
//! taken from a microbenchmark of any one of them is a ranking of the microbenchmark, and the
//! efficiency box (§10) is scored on a corpus, so the profile has to be taken there too.
//!
//! One line per navigation, tab-separated: `NAVTRACE <url> <phase>=<ms> …`, in the order the
//! phases were recorded. Nested phases are recorded too, so a reader sums SIBLINGS, never the
//! whole line.

use std::cell::RefCell;
use std::io::Write;

thread_local! {
    static PHASES: RefCell<Vec<(&'static str, f64)>> = const { RefCell::new(Vec::new()) };
}

fn path() -> Option<&'static std::path::PathBuf> {
    static P: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    P.get_or_init(|| std::env::var_os("KORVUS_NAV_TRACE").map(std::path::PathBuf::from)).as_ref()
}

/// Is the trace on? Call sites that would have to build something to report it check this first.
pub fn enabled() -> bool {
    path().is_some()
}

/// Time `f`, recording how long it took under `name` when the trace is on.
pub fn phase<R>(name: &'static str, f: impl FnOnce() -> R) -> R {
    if !enabled() {
        return f();
    }
    let t = std::time::Instant::now();
    let r = f();
    mark(name, t.elapsed().as_secs_f64() * 1000.0);
    r
}

/// Record a duration measured by the caller.
pub fn mark(name: &'static str, ms: f64) {
    if !enabled() {
        return;
    }
    PHASES.with(|p| p.borrow_mut().push((name, ms)));
}

/// Write everything recorded since the last flush as one line, and start again.
pub fn flush(url: &str) {
    let Some(p) = path() else { return };
    let phases = PHASES.with(|p| std::mem::take(&mut *p.borrow_mut()));
    if phases.is_empty() {
        return;
    }
    let mut line = format!("NAVTRACE\t{url}");
    for (name, ms) in phases {
        line.push_str(&format!("\t{name}={ms:.3}"));
    }
    line.push('\n');
    if let Ok(mut fh) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
        let _ = fh.write_all(line.as_bytes());
    }
}
