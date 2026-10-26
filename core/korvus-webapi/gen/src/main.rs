//! `webapi` — the test-kit CLI.
//!
//! Subcommands:
//!   webapi dump                 parse the vendored geometry corpus and print its interfaces (P0 seed).
//!   webapi test <Iface>         run ONLY that interface's WPT files (fast; the P3 isolated test).
//!   webapi test --all           run the whole WPT corpus.
//!   webapi quality / gencases / status / deps / wpt-sections   (the test kit; see each fn)

use std::process::{Command, ExitCode};

use std::path::Path;

use webapi_gen::deps::DepGraph;
use webapi_gen::spec::{InterfaceSpec, MemberSpec};
use webapi_gen::{blink, coverage, gencases, merge, oracle, profile, quality, snapshot, status, webidl, wpt};

const GEOMETRY: &str = include_str!("../idl/geometry.idl");

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("dump") => dump_geometry(),
        Some("test") => cmd_test(&args[1..]),
        Some("quality") => cmd_quality(&args[1..]),
        Some("gencases") => cmd_gencases(&args[1..]),
        Some("status") => cmd_status(&args[1..]),
        Some("deps") => cmd_deps(&args[1..]),
        Some("wpt-sections") => cmd_wpt_sections(&args[1..]),
        Some(other) => {
            eprintln!(
                "unknown subcommand `{other}`\n\
                 usage: webapi [dump | gencases <Iface...> [--out <dir>] | status [--wpt <json>] [src-root] | deps [idl-file-or-dir...] | test <Iface> | test --all | quality [<Iface>] [--out <json>] [--snapshot <json>] [--wpt <json>]]"
            );
            ExitCode::FAILURE
        }
    }
}

/// P0 seed: parse the vendored geometry corpus and print its interfaces + members.
fn dump_geometry() -> ExitCode {
    let specs = match webidl::parse(GEOMETRY) {
        Ok(specs) => specs,
        Err(e) => {
            eprintln!("parse error: {e}");
            return ExitCode::FAILURE;
        }
    };
    for s in &specs {
        let inherits = s.inherits.as_deref().map(|p| format!(" : {p}")).unwrap_or_default();
        println!("interface {}{}  ({} members)", s.name, inherits, s.members.len());
        for m in &s.members {
            println!("    {}", describe(m));
        }
    }
    ExitCode::SUCCESS
}

/// Print the WPT spec-section directory names that belong to an interface, one per line.
///
/// `KORVUS_WPT_FILTER` matches basenames, and the `html/` corpus names its directories after HTML
/// spec sections instead -- so a filtered run reaches one of the 177 files that test `<input>`.
/// The tag -> interface table that fixes that is generated from Blink and lives in `wpt.rs`; the
/// quality gate is TypeScript and drives the runner directly, so it asks here rather than carrying
/// a second copy of the table that could drift from this one.
///
///     webapi wpt-sections HTMLInputElement   ->   the-input-element
fn cmd_wpt_sections(rest: &[String]) -> ExitCode {
    let Some(iface) = rest.first() else {
        eprintln!("usage: webapi wpt-sections <Iface>");
        return ExitCode::FAILURE;
    };
    for d in wpt::section_dirs_for(iface) {
        println!("{d}");
    }
    ExitCode::SUCCESS
}

/// P3: run the WPT oracle filtered to one interface (or `--all`), then bucket the dumped json and
/// print each interface's pass-rate. Shells out to the runner with `KORVUS_WPT_FILTER` so only the
/// selected interface's files execute (34s full run -> ~1s of test work for one interface).
fn cmd_test(rest: &[String]) -> ExitCode {
    let filter: Option<&str> = match rest.first().map(String::as_str) {
        None | Some("--all") => None,
        Some(iface) => Some(iface),
    };

    let out = std::env::temp_dir().join("korvus_webapi_wpt.json");
    let mut cmd = Command::new("cargo");
    // One binary per crate holds every integration test (core/korvus-runtime/tests/all.rs), so the
    // scoreboard is selected by NAME FILTER -- `--test wpt` no longer names a target.
    cmd.args(["test", "-p", "korvus-runtime", "--test", "all", "--", "--ignored", "wpt::"])
        .env("KORVUS_WPT_OUT", &out);
    if let Some(iface) = filter {
        cmd.env("KORVUS_WPT_FILTER", iface);
        cmd.env("KORVUS_WPT_SECTIONS", wpt::section_dirs_for(iface).join(","));
    }
    eprintln!(
        "running WPT oracle{}...",
        filter.map(|f| format!(" [{f}]")).unwrap_or_else(|| " [--all]".to_string())
    );
    match cmd.status() {
        Ok(s) if s.success() => {}
        Ok(_) => {
            eprintln!("wpt harness floor failed (the runner reported not-ok)");
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("failed to spawn `cargo test`: {e}");
            return ExitCode::FAILURE;
        }
    }

    let json = match std::fs::read_to_string(&out) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("no KORVUS_WPT_OUT written to {}: {e}", out.display());
            return ExitCode::FAILURE;
        }
    };
    let files = match wpt::parse_dump(&json) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("could not parse wpt json: {e}");
            return ExitCode::FAILURE;
        }
    };
    let buckets = wpt::bucket(&files);

    println!("{:<26} {:>6} {:>6} {:>7} {:>6}", "interface", "pass", "total", "rate", "files");
    for (iface, b) in &buckets {
        let rate = b.rate().map(|r| format!("{r:.1}%")).unwrap_or_else(|| "-".to_string());
        println!("{iface:<26} {:>6} {:>6} {rate:>7} {:>6}", b.passed, b.total, b.files);
    }
    ExitCode::SUCCESS
}

/// The unified per-module quality report. Boots the runtime
/// surface probe (or reads `--snapshot`), runs the WPT oracle (or reads `--wpt`), merges the `@webref`
/// corpus, and diffs the live surface against the two captured references (Brave-151 surface +
/// the shipped bundle values) into one `ModuleQuality` per interface — the single object the Implement/
/// Enhance loop consumes. With `<Iface>`: the full per-member table for that one interface; without:
/// a one-line verdict/rollup per interface the snapshot covers. `--out` writes the JSON.
fn cmd_quality(rest: &[String]) -> ExitCode {
    let mut iface: Option<String> = None;
    let mut out: Option<String> = None;
    let mut snapshot_path: Option<String> = None;
    let mut wpt_path: Option<String> = None;
    let mut behavior_path: Option<String> = None;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--out" => {
                i += 1;
                out = rest.get(i).cloned();
            }
            "--snapshot" => {
                i += 1;
                snapshot_path = rest.get(i).cloned();
            }
            "--wpt" => {
                i += 1;
                wpt_path = rest.get(i).cloned();
            }
            "--behavior" => {
                i += 1;
                behavior_path = rest.get(i).cloned();
            }
            other => iface = Some(other.to_string()),
        }
        i += 1;
    }

    // ①⑤⑥ the runtime snapshot + behavior tallies: read the paths, else shell the heavy harness ONCE
    // (it emits both). `snap` feeds present/native/shape/value; `obs` feeds the ⑥ behavior column.
    let (snap, obs) = match get_snapshot(snapshot_path.as_deref(), behavior_path.as_deref()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let behavior_for = |name: &str| obs.get(name);

    // ⑤ WPT: read `--wpt`, else shell the oracle (filtered to the interface when given). Bucketed both
    // per interface (fallback) and per member (Step 1 — per-member spec grading).
    let wpt_files = match get_wpt(wpt_path.as_deref(), iface.as_deref()) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    // ① reference: the merged `@webref` corpus (default `{CARGO_MANIFEST_DIR}/webref`). Loaded BEFORE
    // the WPT buckets, because the bucketer resolves file names against it (A).
    let corpus = merge::parse_corpus(&Path::new(env!("CARGO_MANIFEST_DIR")).join("webref"));
    if corpus.is_empty() {
        eprintln!("no interfaces parsed from the webref corpus (is gen/webref/ vendored?)");
        return ExitCode::FAILURE;
    }
    let vocab = wpt::Vocab::from_corpus(&corpus);
    let buckets = wpt_files.as_ref().map(|f| wpt::bucket_with(f, Some(&vocab)));
    let member_buckets = wpt_files.as_ref().map(|f| wpt::bucket_by_member_with(f, Some(&vocab)));

    // ③④ references. Anchored on the REPO ROOT rather than counted out in `..` hops from
    // CARGO_MANIFEST_DIR: this crate is three levels down (`core/korvus-webapi/gen`), and the two
    // references live in different subtrees, so the hop counts differed from each other and neither
    // survived the crate moving. The persona path did not: it still said `../../korvus-persona`,
    // which resolved correctly while this crate sat under `plugins/`, and silently pointed at
    // `core/korvus-persona` -- a directory that does not exist -- once it moved to `core/`.
    //
    // It failed loudly here and quietly one level up: `coverage.sh` printed "per-interface grading:
    // <path>" and exited 0 for a run that produced no file at all. Both halves are fixed; this is
    // the half that stops the path from being a hop count in the first place.
    // The surface baseline is the shipped bundle's `surface` artifact, found through the persona
    // crate (the bundle is one directory and its manifest is the index).
    let baseline_path = persona::shipped_artifact("surface");
    // The values come from the persona crate itself, not from a path spelled out here. This line
    // used to name `the shipped bundle's values.json`, so the day the shipped bundle
    // changed the grader carried on grading against a device the engine had stopped wearing --
    // no error, no warning, just a stale denominator.
    let baseline: serde_json::Value = match std::fs::read_to_string(&baseline_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
    {
        Some(v) => v,
        None => {
            eprintln!("could not read/parse the surface baseline at {}", baseline_path.display());
            return ExitCode::FAILURE;
        }
    };
    let prof = match profile::Profile::from_json(persona::SHIPPED_VALUES) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("the persona crate's embedded values do not parse: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Grade the requested interface, or every corpus interface the snapshot covers.
    let bucket_for = |name: &str| buckets.as_ref().and_then(|b| b.get(name));
    let members_for = |name: &str| member_buckets.as_ref().and_then(|b| b.get(name));
    match &iface {
        Some(name) => {
            let Some(spec) = corpus.get(name) else {
                eprintln!("interface `{name}` is not in the webref corpus");
                return ExitCode::FAILURE;
            };
            let mut report = quality::assess(name, spec, &snap, &baseline, &prof, bucket_for(name), members_for(name), behavior_for(name));
            // (E) Attach the failing upstream subtests. These lead the implementer's worklist: an
            // external "the spec requires X and you do Y" beats anything the implementer would have
            // thought to test about its own code.
            if let Some(files) = wpt_files.as_ref() {
                report.wpt_failures = wpt::failures_for(files, name, Some(&vocab), 40);
            }
            // Blink's implementation of this interface, when a checkout is available. WebIDL says a
            // member EXISTS and what it is typed as; the algorithm lives in prose and in the engines,
            // and for a browser whose goal is to be indistinguishable from Chrome, Chrome's engine is
            // the more faithful of those two references.
            if let Some(blink_root) = blink::root_from_env() {
                if let Some(r) = blink::index(&blink_root).get(name) {
                    report.blink_refs = r.paths().into_iter().map(String::from).collect();
                }
            }
            print!("{}", render_quality(&report));
            if let Some(out) = out {
                if let Err(e) = write_json(&out, &report) {
                    eprintln!("write {out}: {e}");
                    return ExitCode::FAILURE;
                }
                println!("\nwrote {out}");
            }
        }
        None => {
            // Summary across every corpus interface the snapshot actually exposes.
            let mut reports: std::collections::BTreeMap<String, quality::ModuleQuality> = std::collections::BTreeMap::new();
            for (name, spec) in &corpus {
                if !snap.has_interface(name) {
                    continue;
                }
                reports.insert(name.clone(), quality::assess(name, spec, &snap, &baseline, &prof, bucket_for(name), members_for(name), behavior_for(name)));
            }
            println!("{:<28} {:<13} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}", "interface", "verdict", "present", "native", "shape", "value", "wpt", "behavior", "spec", "verified");
            println!("{}", "-".repeat(119));
            for (name, r) in &reports {
                println!(
                    "{:<28} {:<13} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
                    name, r.verdict, r.rollup.present, r.rollup.native, r.rollup.shape, r.rollup.value, r.rollup.wpt, r.rollup.behavior, r.rollup.spec, r.rollup.verified
                );
            }
            println!("\n{} interfaces graded (snapshot ∩ corpus)", reports.len());
            if let Some(out) = out {
                if let Err(e) = write_json(&out, &reports) {
                    eprintln!("write {out}: {e}");
                    return ExitCode::FAILURE;
                }
                println!("wrote {out}");
            }
        }
    }
    ExitCode::SUCCESS
}

/// Read the surface snapshot + behavior tallies from paths, else shell the heavy `quality` harness ONCE
/// (in core/korvus-webapi/tests; the harness emits BOTH `KORVUS_SNAPSHOT_OUT` and
/// `KORVUS_BEHAVIOR_OUT` before its floor-only asserts). The obs map feeds the ⑥ behavior column.
fn get_snapshot(
    snapshot_path: Option<&str>,
    behavior_path: Option<&str>,
) -> Result<(snapshot::SurfaceSnapshot, oracle::ObsMap), String> {
    if let Some(p) = snapshot_path {
        let snap = snapshot::SurfaceSnapshot::from_path(Path::new(p))
            .map_err(|e| format!("read snapshot {p}: {e}"))?;
        let obs = behavior_path.map(|b| oracle::read_obs(Path::new(b))).unwrap_or_default();
        return Ok((snap, obs));
    }
    let snap_out = std::env::temp_dir().join("korvus_webapi_snapshot.json");
    let beh_out = std::env::temp_dir().join("korvus_webapi_behavior.json");
    eprintln!("booting the runtime surface probe (heavy; pass --snapshot <json> to skip)...");
    // `--nocapture` is REQUIRED: libtest's per-thread stdout capture deadlocks with the engine's V8/tokio
    // worker threads (the harness hangs at 0% CPU without it). The harness emits to the env-var paths, so
    // letting its prints through is harmless.
    let status = Command::new("cargo")
        // `--test all` + a name filter: quality.rs is a module of the crate's one test binary.
        .args(["test", "-p", "korvus-webapi", "--test", "all", "--", "--ignored", "--nocapture", "quality::"])
        .env("KORVUS_SNAPSHOT_OUT", &snap_out)
        .env("KORVUS_BEHAVIOR_OUT", &beh_out)
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(_) => return Err("quality harness probe failed (the runner reported not-ok)".to_string()),
        Err(e) => return Err(format!("failed to spawn `cargo test` for the snapshot: {e}")),
    }
    let snap = snapshot::SurfaceSnapshot::from_path(&snap_out)
        .map_err(|e| format!("read snapshot {}: {e}", snap_out.display()))?;
    Ok((snap, oracle::read_obs(&beh_out)))
}

/// Read WPT buckets from `--wpt`, else shell the WPT oracle (filtered to `iface` when given, for a
/// ~1s run instead of the full 34s) into a temp file and bucket it. Mirrors cmd_test.
fn get_wpt(
    path: Option<&str>,
    iface: Option<&str>,
) -> Result<Option<Vec<wpt::FileResult>>, String> {
    if let Some(p) = path {
        let json = std::fs::read_to_string(p).map_err(|e| format!("read wpt json {p}: {e}"))?;
        let files = wpt::parse_dump(&json).map_err(|e| format!("parse wpt json {p}: {e}"))?;
        return Ok(Some(files));
    }
    let out = std::env::temp_dir().join("korvus_webapi_quality_wpt.json");
    let _ = std::fs::remove_file(&out); // clear any stale dump so a spawn failure can't read old data
    let mut cmd = Command::new("cargo");
    cmd.args(["test", "-p", "korvus-runtime", "--test", "all", "--", "--ignored", "--nocapture", "wpt::"]).env("KORVUS_WPT_OUT", &out);
    if let Some(iface) = iface {
        cmd.env("KORVUS_WPT_FILTER", iface);
        // (B) The name filter matches basenames; the html/ tree names its directories after spec
        // sections instead, so a filtered run would skip every file that tests this interface there.
        cmd.env("KORVUS_WPT_SECTIONS", wpt::section_dirs_for(iface).join(","));
    }
    eprintln!("running WPT oracle{}...", iface.map(|f| format!(" [{f}]")).unwrap_or_else(|| " [--all]".to_string()));
    let status = cmd.status();
    // The runner writes its scoreboard JSON in pass 1 (always), THEN a pass-2 floor assertion guards
    // the harness (not conformance) -- an interface WPT doesn't cover by name matches 0 files and trips
    // that floor. WPT is one optional axis here, so we consume the written dump whenever it exists and
    // only hard-fail if it truly wasn't produced.
    match &status {
        Ok(s) if !s.success() => {
            eprintln!("note: WPT runner returned not-ok (harness floor; often 0 files for this filter) -- using the dump anyway");
        }
        Err(e) => return Err(format!("failed to spawn `cargo test` for wpt: {e}")),
        _ => {}
    }
    let json = match std::fs::read_to_string(&out) {
        Ok(j) => j,
        Err(e) => {
            // No dump AND a non-ok run means the harness never got to write it -- that IS fatal.
            return Err(format!("no KORVUS_WPT_OUT at {} (WPT run produced no scoreboard): {e}", out.display()));
        }
    };
    let files = wpt::parse_dump(&json).map_err(|e| format!("parse wpt json: {e}"))?;
    Ok(Some(files))
}

/// Serialize any value pretty to a path.
fn write_json<T: serde::Serialize>(path: &str, value: &T) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, json)
}

/// The per-member table + rollup + verdict + worklist for one interface's report.
fn render_quality(q: &quality::ModuleQuality) -> String {
    let mut out = String::new();
    out.push_str(&format!("== {} ==   verdict: {}\n", q.iface, q.verdict.to_uppercase()));
    out.push_str(&format!(
        "rollup: present {}  native {}  shape {}  value {}  wpt {}  behavior {}  spec {}  VERIFIED {}  (external {} · const {} · behavioral {})\n\n",
        q.rollup.present, q.rollup.native, q.rollup.shape, q.rollup.value, q.rollup.wpt, q.rollup.behavior, q.rollup.spec, q.rollup.verified, q.rollup.independent, q.rollup.consts, q.rollup.behavioral
    ));
    out.push_str(&format!("{:<34} {:<6} {:<7} {:<7} {:<16} {:<8} {:<9} {:<7} {}\n", "member", "①pres", "②nat", "③shape", "④value", "⑤wpt", "⑥behav", "⑦spec", "verified"));
    out.push_str(&"-".repeat(116));
    out.push('\n');
    for m in &q.members {
        let pres = if m.present { "yes" } else { "MISS" };
        let nat = if !m.present {
            "-".to_string()
        } else if m.native.ok {
            "ok".to_string()
        } else if !m.native.native {
            "LIE".to_string()
        } else if m.native.own_tell {
            "tell".to_string()
        } else {
            "len".to_string()
        };
        let shape = if !m.present {
            "-".to_string()
        } else if m.shape.matches {
            "ok".to_string()
        } else {
            "DIFF".to_string()
        };
        let val = match (&m.value.profile, &m.value.ours) {
            (Some(_), Some(o)) => {
                let mark = if m.value.from_profile { "=ok" } else { "DRIFT" };
                format!("{}{mark}", trunc(&serde_json::to_string(o).unwrap_or_default(), 10))
            }
            (Some(_), None) => "unmeasured".to_string(),
            _ => "-".to_string(),
        };
        let beh = if !m.behavior.tested {
            "-".to_string()
        } else if m.behavior.ok {
            format!("ok {}/{}", m.behavior.pass, m.behavior.total)
        } else {
            format!("FAIL {}/{}", m.behavior.pass, m.behavior.total)
        };
        let sp = if !m.present {
            "-".to_string()
        } else if m.spec.ok {
            "ok".to_string()
        } else if !m.spec.kind_ok {
            "KIND".to_string()
        } else if !m.spec.readonly_ok {
            "RDONLY".to_string()
        } else {
            "CONST".to_string()
        };
        let vf = if !m.present { "-" } else if m.verified { "yes" } else { "surface" };
        out.push_str(&format!("{:<34} {:<6} {:<7} {:<7} {:<16} {:<8} {:<9} {:<7} {}\n", trunc(&m.name, 34), pres, nat, shape, val, m.wpt, beh, sp, vf));
    }
    if !q.worklist.is_empty() {
        out.push_str("\nworklist:\n");
        for w in &q.worklist {
            out.push_str(&format!("  - {w}\n"));
        }
    }
    out
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        format!("{}…", s.chars().take(n.saturating_sub(1)).collect::<String>())
    } else {
        s.to_string()
    }
}

/// Generate behavior-case SKELETONS (`{iface, member, expr}`, no `expected`) from the WebIDL corpus.
///
/// The output is deliberately unusable on its own: the next capture runs each
/// expression in a real Chrome, twice, keeps the ones that agree with themselves, and writes the
/// answer in. Generating the QUESTION here and recording the ANSWER there leaves the implementer in
/// charge of neither.
fn cmd_gencases(rest: &[String]) -> ExitCode {
    let out_dir = rest
        .iter()
        .position(|a| a == "--out")
        .and_then(|i| rest.get(i + 1))
        .cloned();
    let wanted: Vec<&String> = rest.iter().take_while(|a| !a.starts_with("--")).collect();
    if wanted.is_empty() {
        eprintln!("usage: webapi gencases <Iface...> [--out <dir>]");
        return ExitCode::FAILURE;
    }
    // The same merged `@webref` corpus the quality tester grades against, so a generated case can
    // never ask about a member the grader does not know exists.
    let specs = merge::parse_corpus(&Path::new(env!("CARGO_MANIFEST_DIR")).join("webref"));
    if specs.is_empty() {
        eprintln!("no interfaces parsed from the webref corpus (is gen/webref/ vendored?)");
        return ExitCode::FAILURE;
    }
    let mut total = 0usize;
    for want in &wanted {
        let Some(spec) = specs.get(want.as_str()) else {
            eprintln!("{want}: not in the WebIDL corpus");
            continue;
        };
        let mut cases = gencases::generate(spec);
        cases.extend(gencases::coercion_probes(spec));
        total += cases.len();
        let json = serde_json::to_string_pretty(&cases).unwrap_or_default();
        match &out_dir {
            Some(d) => {
                let path = std::path::Path::new(d).join(format!("{want}.json"));
                if let Err(e) = std::fs::create_dir_all(d).and_then(|_| std::fs::write(&path, json + "\n")) {
                    eprintln!("write {}: {e}", path.display());
                    return ExitCode::FAILURE;
                }
                println!("{want}: {} case(s) -> {}", cases.len(), path.display());
            }
            None => println!("{json}"),
        }
    }
    println!("generated {total} skeleton case(s) across {} interface(s)", wanted.len());
    ExitCode::SUCCESS
}

/// P5: one dashboard across interface families — static impl% (from the scaffold source) merged
/// with WPT pass-rate (from a `--wpt <KORVUS_WPT_OUT.json>`, optional) and next-to-implement stubs.
/// Defaults to scanning the korvus-webapi `src` tree.
fn cmd_status(rest: &[String]) -> ExitCode {
    let mut wpt_json: Option<String> = None;
    let mut src_root: Option<String> = None;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--wpt" => {
                i += 1;
                if let Some(p) = rest.get(i) {
                    wpt_json = Some(p.clone());
                }
            }
            other => src_root = Some(other.to_string()),
        }
        i += 1;
    }
    let src_root =
        src_root.unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../src").to_string_lossy().into_owned());

    let cov = match coverage::scan_dir(Path::new(&src_root)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("scan {src_root}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let buckets = match &wpt_json {
        Some(p) => match std::fs::read_to_string(p) {
            Ok(j) => match wpt::parse_dump(&j) {
                Ok(files) => Some(wpt::bucket(&files)),
                Err(e) => {
                    eprintln!("parse wpt json {p}: {e}");
                    return ExitCode::FAILURE;
                }
            },
            Err(e) => {
                eprintln!("read wpt json {p}: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };
    print!("{}", status::render(&status::build(&cov, buckets.as_ref())));
    ExitCode::SUCCESS
}

/// Print the interface dependency layers (leaves first; each layer is an independent/parallel set)
/// from the WebIDL under the given paths (files or dirs). Defaults to the vendored `gen/idl`.
fn cmd_deps(rest: &[String]) -> ExitCode {
    let roots: Vec<String> = if rest.is_empty() {
        vec![format!("{}/idl", env!("CARGO_MANIFEST_DIR"))]
    } else {
        rest.to_vec()
    };
    let mut idl_files: Vec<std::path::PathBuf> = Vec::new();
    for r in &roots {
        let p = Path::new(r);
        if p.is_dir() {
            if let Ok(entries) = std::fs::read_dir(p) {
                for e in entries.flatten() {
                    let path = e.path();
                    if path.extension().and_then(|x| x.to_str()) == Some("idl") {
                        idl_files.push(path);
                    }
                }
            }
        } else if p.is_file() {
            idl_files.push(p.to_path_buf());
        }
    }
    idl_files.sort();

    let mut specs: Vec<InterfaceSpec> = Vec::new();
    let mut skipped = 0usize;
    // weedle PANICS (not Err) on files it can't fully consume (callbacks / typedefs / partials /
    // trailing junk), so catch per-file unwinds under a silenced hook and just skip those files.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    for f in &idl_files {
        let parsed = std::fs::read_to_string(f).ok().and_then(|src| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| webidl::parse(&src).ok()))
                .ok()
                .flatten()
        });
        match parsed {
            Some(mut s) => specs.append(&mut s),
            None => skipped += 1,
        }
    }
    std::panic::set_hook(prev_hook);
    if specs.is_empty() {
        eprintln!("no interfaces parsed from {} idl file(s) ({skipped} skipped)", idl_files.len());
        return ExitCode::FAILURE;
    }

    let graph = DepGraph::build(&specs);
    // `--json` emits the IMPLEMENTATION order (inheritance layers) plus the soft edges, which is
    // what the layered campaign consumes: the layers say what to work and in what order, the edges
    // say who to re-grade once a layer lands. The human-readable print below keeps showing both
    // graphs, because seeing the type-reference graph collapse into one cyclic blob is the argument
    // for why the implementation order is inheritance-only.
    if rest.iter().any(|a| a == "--json") {
        let impl_layers = graph.implementation_layers();
        let deps: std::collections::BTreeMap<&str, Vec<&str>> = graph
            .deps
            .iter()
            .map(|(k, v)| (k.as_str(), v.iter().map(String::as_str).collect()))
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "layers": impl_layers,
                "inherits": graph.inherits,
                "deps": deps,
                "parsed": specs.len(),
            })
        );
        return ExitCode::SUCCESS;
    }

    let layered = graph.layers();
    println!(
        "dependency layers (leaves first; each layer is an independent/parallel set)\n\
         parsed {} interfaces from {} idl file(s), {skipped} unparseable file(s) skipped\n",
        specs.len(),
        idl_files.len()
    );
    for (i, layer) in layered.layers.iter().enumerate() {
        println!("layer {i} ({}): {}", layer.len(), layer.join(", "));
    }
    let impl_layers = graph.implementation_layers();
    println!(
        "\nIMPLEMENTATION ORDER (inheritance only -- acyclic, {} layers):",
        impl_layers.len()
    );
    for (i, layer) in impl_layers.iter().enumerate() {
        let head: Vec<&str> = layer.iter().take(10).map(String::as_str).collect();
        let more = layer.len().saturating_sub(head.len());
        println!(
            "  d{i} ({}): {}{}",
            layer.len(),
            head.join(", "),
            if more > 0 { format!(" +{more} more") } else { String::new() }
        );
    }

    if !layered.cyclic.is_empty() {
        println!("\ncyclic ({}, migrate together): {}", layered.cyclic.len(), layered.cyclic.join(", "));
    }
    ExitCode::SUCCESS
}

fn describe(m: &MemberSpec) -> String {
    match m {
        MemberSpec::Constructor { args, .. } => format!("constructor({} args)", args.len()),
        MemberSpec::Attribute { name, ty, readonly, is_static, inherited, .. } => {
            let mut flags = Vec::new();
            if *is_static {
                flags.push("static");
            }
            if *inherited {
                flags.push("inherit");
            }
            if *readonly {
                flags.push("readonly");
            }
            let prefix = if flags.is_empty() { String::new() } else { format!("{} ", flags.join(" ")) };
            format!("{prefix}attribute {ty} {name}")
        }
        MemberSpec::Operation { name, ret, args, is_static } => {
            let prefix = if *is_static { "static " } else { "" };
            format!("{prefix}{ret} {name}({} args)", args.len())
        }
        MemberSpec::Const { name, ty, value } => format!("const {ty} {name} = {value}"),
        MemberSpec::Special { special } => format!("special {} (named property)", special.member_name()),
    }
}
