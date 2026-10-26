//! `KORVUS_VERIFY_INCREMENTAL`: every pass that reuses work checked against one that reuses none.
//!
//! When it is set, each style-and-layout pass of [`crate::Page::layout_document`] is run a second time
//! from nothing -- no matched rules carried over, no style or layout of the previous pass -- and the
//! two are compared: every element's [`crate::style::StyleKey`], every element's generated content,
//! and every box (its node, style, parent and children, text, and its geometry to the bit). The pass
//! that reuses work has to give the answer the one that reuses none gives, and this is how each
//! incremental path proves it. Each animation rescan of
//! [`crate::Page::declarative_update`] that starts from what changed is checked the same way against
//! the whole-document rescan: the animations, the transitions started and cancelled, and
//! every element's before-change style.
//!
//! - `KORVUS_VERIFY_INCREMENTAL=1` panics on the first difference: for tests.
//! - `KORVUS_VERIFY_INCREMENTAL=log:<file>` appends one `MISMATCH` line per difference to `<file>` and
//!   goes on: for fixture runs and replay sweeps, where a panic would lose the rest of the page. Each
//!   process that verifies anything also writes one `VERIFYING` line ([`started`]).
//!
//! The second pass costs as much as the first, so a verified load takes about twice the style and
//! layout time. It is never on by default.

use std::io::Write as _;
use std::sync::OnceLock;

enum Mode {
    Off,
    Panic,
    Log(String),
}

fn mode() -> &'static Mode {
    static MODE: OnceLock<Mode> = OnceLock::new();
    MODE.get_or_init(|| match std::env::var("KORVUS_VERIFY_INCREMENTAL") {
        Ok(v) if v.is_empty() || v == "0" => Mode::Off,
        Ok(v) => match v.strip_prefix("log:") {
            Some(path) => Mode::Log(path.to_string()),
            None => Mode::Panic,
        },
        Err(_) => Mode::Off,
    })
}

/// Is verification on?
pub(crate) fn enabled() -> bool {
    forced() || !matches!(mode(), Mode::Off)
}

#[cfg(test)]
thread_local! {
    /// Verification on, panicking, for this thread whatever the environment says: for a test that
    /// checks every pass it makes.
    pub(crate) static FORCED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn forced() -> bool {
    #[cfg(test)]
    return FORCED.with(std::cell::Cell::get);
    #[cfg(not(test))]
    false
}

/// Note that this process verifies: in log mode, one `VERIFYING` line per process, at its first verified
/// pass, so a sweep's log shows which runs were checked as well as what they found. A log with no such
/// line from a run that laid something out means the setting never reached it.
pub(crate) fn started() {
    static ONCE: OnceLock<()> = OnceLock::new();
    if let Mode::Log(_) = mode() {
        ONCE.get_or_init(|| log("VERIFYING"));
    }
}

fn log(what: &str) {
    let Mode::Log(path) = mode() else { return };
    // One `write` per line, so the processes of a sweep can share the file.
    static RUN: OnceLock<String> = OnceLock::new();
    let run = RUN.get_or_init(|| {
        let args: Vec<String> = std::env::args().skip(1).collect();
        format!("pid {} [{}]", std::process::id(), args.join(" ").chars().take(240).collect::<String>())
    });
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(format!("{run} {what}\n").as_bytes());
    }
}

/// Report a difference between the pass that reused work and the one that did not.
pub(crate) fn mismatch(what: &str) {
    crate::perf_counters::add(&crate::perf_counters::VERIFY_MISMATCHES, 1);
    if forced() {
        panic!("incremental != full: {what}");
    }
    match mode() {
        Mode::Off => {}
        Mode::Panic => panic!("KORVUS_VERIFY_INCREMENTAL: incremental != full: {what}"),
        Mode::Log(_) => log(&format!("MISMATCH {what}")),
    }
}

/// `tag#id.class` of an element, for a report.
pub(crate) fn describe(dom: &korvus_dom::Dom, node: korvus_dom::NodeId) -> String {
    let Some(tag) = dom.tag_name(node) else { return format!("#{node} (not an element)") };
    let mut out = format!("#{node} <{tag}");
    if let Some(id) = dom.get_attribute(node, "id").filter(|v| !v.is_empty()) {
        out.push_str(&format!(" id={id:?}"));
    }
    if let Some(class) = dom.get_attribute(node, "class").filter(|v| !v.is_empty()) {
        let class: String = class.chars().take(80).collect();
        out.push_str(&format!(" class={class:?}"));
    }
    out.push('>');
    out
}

/// The page's `<title>` text, to tell a sweep's runs apart in a report.
pub(crate) fn document_title(dom: &korvus_dom::Dom) -> String {
    let title = dom.get_elements_by_tag_name("title").into_iter().find(|&t| dom.root_of(t) == dom.root());
    let text = title.map(|t| dom.text_content(t)).unwrap_or_default();
    format!("{:?}", text.trim().chars().take(60).collect::<String>())
}

/// What led to a difference at `node` ("the mutation history"): its ancestors, and the last
/// journal entries that name it or one of them.
pub(crate) fn history(dom: &korvus_dom::Dom, node: korvus_dom::NodeId) -> String {
    use korvus_dom::journal::Change;
    let chain: Vec<korvus_dom::NodeId> = std::iter::successors(Some(node), |&n| dom.parent(n).or_else(|| dom.shadow_host_of(n))).take(64).collect();
    let names: Vec<String> = chain.iter().skip(1).take(4).map(|&n| describe(dom, n)).collect();
    let journal = dom.journal();
    let from = journal.seq().saturating_sub(4096);
    let touches = |c: &Change| match c {
        Change::Attribute { node, .. } | Change::Text { node } => chain.contains(node),
        Change::Inserted { parent, node } | Change::Removed { parent, node, .. } => chain.contains(parent) || chain.contains(node),
        Change::ChildrenReplaced { parent, old } => chain.contains(parent) || journal.nodes(*old).iter().any(|o| chain.contains(o)),
        Change::State { node } => node.is_some_and(|n| chain.contains(&n)),
        Change::Hover { old } => journal.nodes(*old).iter().chain(dom.hover_chain()).any(|n| chain.contains(n)),
        Change::Other { .. } => true,
    };
    let recent: Vec<String> = journal
        .since(from)
        .unwrap_or(&[])
        .iter()
        .filter(|c| touches(c))
        .rev()
        .take(8)
        .map(|c| format!("{c:?}").chars().take(160).collect())
        .collect();
    format!("under {}; journal (latest first): {}", names.join(" < "), recent.join(" | "))
}
