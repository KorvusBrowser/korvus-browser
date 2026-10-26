//! WPT (Web Platform Tests) conformance scoreboard -- Phase A.2 broad corpus.
//!
//! Runs a broad curated subset of REAL, vendored WPT test files (fixtures/wpt/**, curled verbatim from
//! web-platform-tests/wpt@master) through Korvus's `Engine` via the SAME code path a real navigation
//! uses: `Engine::load_page`, which fetches every `<script src>` (testharness.js, the report hook,
//! same-dir + META helpers, and the test body itself) over the stealth transport. Those fetches are
//! satisfied by a LOCAL, path-routing static file server bound to 127.0.0.1:0 that maps a URL path to a
//! file under `tests/fixtures/wpt/` -- no wptserve, no /etc/hosts, no 30k-file clone, no network. This
//! is A.2 upgrade over A.1's in-process inject harness: because resources load naturally over
//! transport, any STATIC testharness-based test runs unmodified, so the corpus scales from a handful to
//! dozens of files and the pass-rate becomes a meaningful capability signal again.
//!
//! It is a SCOREBOARD, not a pass/fail gate: individual subtest failures are the signal (an honest
//! capability gap = the C2 backlog), so they are reported, not fatal. The floor assertion only guards
//! that the harness itself works (real testharness.js loads; some subtests run and pass).
//!
//! Report hook: every WPT test loads `/resources/testharness.js` then `/resources/testharnessreport.js`.
//! wptserve's testharnessreport.js posts results to the runner; we serve a Korvus hook in its place
//! (fixtures/wpt/resources/testharnessreport.js) that captures results in-process onto
//! `globalThis.__wptresults`. It runs immediately after testharness.js, exactly where the harness
//! globals it uses already exist -- no test content is modified.
//!
//! Completion mechanics (see testharness.js): a window test finalizes when `load` fires AND every
//! subtest settles. `load_page` dispatches DOMContentLoaded + load itself (load on a 0ms task), so the
//! runner just pumps. The report hook set `explicit_timeout` (disabling the harness auto-timeout), so if
//! an async subtest is still pending after the pump budget we call the global `timeout()` to force it to
//! resolve as a failure (honest) rather than blackhole the whole file, then read the completion payload
//! -- or fall back to the incrementally captured partial. That force-finalisation is UNCONDITIONAL: it
//! used to be skipped under `KORVUS_WPT_a reference engine=1`, which is the flag the like-for-like sweep runs
//! with, so on exactly the measured run nothing finalised and a stopped-early file left the denominator
//! at 0/0 instead of entering it at 0/N. Removing that gate lowers the headline pass rate on purpose.
use engine::Engine;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// Spec-fixed conformance (self-asserting WPT files) -- persona is irrelevant, so use the proven stub
// (mirrors obstacle_course.rs). The captured-Brave persona only matters for oracle-backed stages.
/// Force any still-pending async subtest to resolve (as TIMEOUT -> a failure, honestly) and fire the
/// completion callbacks. Effective because the report hook set explicit_timeout (timeout_length=null).
const FORCE_TIMEOUT: &str = r#"try { timeout(); } catch (e) {}"#;

/// Hard per-file wall-clock deadline. Some WPT files drive Korvus into an UNINTERRUPTIBLE synchronous
/// loop (measured: 100% CPU, no progress for minutes) -- a genuine engine bug where a page script or a
/// testharness path never yields, so the cooperative `tokio::time::timeout` around the async pump can't
/// bound it. A watchdog thread calls `IsolateInterrupt::terminate()` at this deadline to break the loop,
/// so one hanging file can't blackhole the whole scoreboard; the file is recorded as TERMINATED with
/// whatever subtests settled before the hang. Generous enough not to false-fire on merely-slow work.
///
/// 4000 -> 8000 (with the pump budget 1200 -> 2500, below). A stratified 80-file sample of the
/// no-verdict pool, re-run at `KORVUS_WPT_DEADLINE_MS=15000`, converted 13; SIX of the thirteen
/// finished at ~1.71s -- barely past the old 1200ms pump -- and three more at ~7.2s. So the tail that
/// time can buy is short and cheap, and matching a reference engine's flat 15000ms everywhere would cost ~12x the
/// sweep's wall clock to reach the SAME thirteen files. This is the smallest pair of numbers that
/// covers the measured cluster.
const HARD_DEADLINE_MS: u64 = 8000;

/// How long a file that already blew `HARD_DEADLINE_MS` gets to FINALISE before the watchdog kills it
/// again. It covers exactly the steps after the pump loop: `clear_termination()`, four short
/// `eval_value` read-backs, the `timeout()` force-finalisation, and one 300 ms settle pump. All of that
/// is well under a second on any file; only a file whose loop came back to life after
/// `clear_termination()` can still be running when the grace is up, and that file must be killed again
/// rather than left to run. See the watchdog's block comment in `run_file` for the measurement that
/// made this necessary. Applies ONLY after a kill -- a file that finishes normally never reaches it, so
/// it costs the sweep nothing.
const FINALIZE_GRACE_MS: u64 = 2000;

/// The pump loop's own budget -- how long a file gets to publish `__wptresults` before the runner
/// finalises it. Always below `HARD_DEADLINE_MS`, so the loop's deadline (which leaves a live isolate)
/// is what ends a slow file rather than the watchdog's `terminate()` (which poisons it).
const PUMP_BUDGET_MS: u64 = 2500;

/// `<meta name="timeout" content="long">` widens BOTH budgets by this factor.
///
/// This is what wptrunner does and what testharness.js itself does: `WindowTestEnvironment.test_timeout`
/// reads that meta and returns `settings.harness_timeout.long` (60s) instead of `.normal` (10s), a 6x
/// step. The files that carry it are the ones that legitimately need the wall clock --
/// `html/semantics/scripting-1/moving-between-documents/*` and the `xhr/xmlhttprequest-timeout-*`
/// family, whose sub-cases sleep for seconds by design. 3x (7.5s pump, 24s watchdog) rather than 6x
/// because the measured converters top out at ~7.2s and the rest of the factor is pure sweep time.
const LONG_TIMEOUT_MULTIPLIER: u64 = 3;

/// TRIED AND MEASURED NOT TO HELP -- kept at the SAME value as `HARD_DEADLINE_MS`, not because a wider
/// worker-scope deadline is wrong in principle but because it was tested and the data said no.
///
/// a reference engine's own `test_timeout_ms` defaults to 15000ms
/// (a reference implementation:79-81) against our 1200ms pump budget --
/// a real 12.5x gap, and matching it looked like removing an unfairness rather than gaming the
/// comparison. It was tried: `worker_scope_actually_runs` files given 15000ms (this constant, then set
/// to 15000) instead of `HARD_DEADLINE_MS`. Measured, 6 shards, core tier: `.any.worker.html` no-verdict
/// 403/420 (was 380/400 at 1200ms -- WORSE, not better) and `.any.sharedworker.html` no-verdict
/// unchanged at 176/178; every one of those 579 remaining worker-scope no-verdict files had
/// `harness_loaded: true` (testharness.js ran, `typeof test === 'function'`) -- so they are not timing
/// out on the network+thread-spawn round trip this constant was meant to cover; something else stops
/// them completing, and 14.5 extra seconds does not fix it. Total sweep wall-clock went from ~885s to
/// 2372s (2.7x) for that. Kept equal to `HARD_DEADLINE_MS` rather than deleted, as the record of the
/// experiment and the place a future attempt (with the ACTUAL blocker identified first) should look.
const WORKER_HARD_DEADLINE_MS: u64 = HARD_DEADLINE_MS;

/// A one-off, env-gated GLOBAL override of the per-file deadline (watchdog + pump budget), for a single
/// measurement a peer session asked for after `WORKER_HARD_DEADLINE_MS`'s doc comment: that experiment
/// widened ONLY worker-scope files, which instrumentation later showed are blocked on something
/// structural (99% register zero tests even alone on an idle machine) rather than on time -- so
/// "widening did not help" was measured on exactly the one population it could not have helped. The
/// much larger non-worker no-verdict pool (2,333 files, 1,350 of which DO register real tests and are
/// still running when the clock stops them) has never been tested. `KORVUS_WPT_DEADLINE_MS=15000`
/// (matching a reference engine's own `test_timeout_ms`, see `WORKER_HARD_DEADLINE_MS`'s comment) runs that test,
/// uniformly, every file. Unset (the default) leaves everything exactly as measured/reverted above --
/// this is deliberately NOT a new default; per the peer's own instruction, if it helps, the decision to
/// keep it belongs to them, not to this unit picking the more flattering number.
fn global_deadline_override() -> Option<u64> {
    std::env::var("KORVUS_WPT_DEADLINE_MS").ok().and_then(|v| v.parse().ok())
}

/// Percent-encode a variant string for the one place WPT puts characters a URL cannot carry
/// literally: `<meta name=variant content="?load fires normally">`.
///
/// A variant is written for humans and we copied it verbatim into the URL. That put a raw space in
/// `http_get`'s request line -- `GET /xhr/xmlhttprequest-timeout-simple.html?load fires normally
/// HTTP/1.1` -- which is four space-separated tokens where HTTP/1.1 allows exactly three. wptserve
/// answers `400 Bad request syntax` (verified against the live server), so what the engine got back
/// was an error page with no `<script src=testharness.js>` in it, and the file reached no verdict at
/// all. Measured: 30 core files, every one of them in the `harness-never-loaded` bucket.
///
/// Encoding is also what the test itself demands, not merely what the wire demands:
/// `xhr/resources/xmlhttprequest-timeout.js:326` picks its sub-case with
/// `decodeURIComponent(location.search.substr(1))`, which only round-trips if the space arrived
/// as `%20`.
///
/// The set encoded is the URL Standard's *query percent-encode set* -- C0 controls, space, `"`, `#`,
/// `<`, `>` -- plus everything non-ASCII. `?`, `&`, `=`, `,` and `+` are deliberately left alone so a
/// variant like `?include=xhr` stays the query it already is.
fn encode_variant(variant: &str) -> String {
    let mut out = String::with_capacity(variant.len());
    for b in variant.bytes() {
        if b <= 0x20 || b >= 0x7f || matches!(b, b'"' | b'#' | b'<' | b'>') {
            out.push_str(&format!("%{b:02X}"));
        } else {
            out.push(b as char);
        }
    }
    out
}

/// Decode a document body to a String, honouring a byte-order mark.
///
/// `String::from_utf8_lossy` over a UTF-16 document replaces the NUL high byte of every ASCII
/// character with U+FFFD, so `<!doctype html>` arrives as `\u{FFFD}<\u{FFFD}!\u{FFFD}d...`: not one
/// tag survives the parser, no `<script>` is ever seen, testharness.js is never fetched, and the file
/// reaches no verdict. `std::fs::read_to_string` fails differently and worse -- it returns Err on
/// non-UTF-8 and the `unwrap_or_default()` above it hands the engine an EMPTY document.
///
/// The BOM is the whole rule here rather than a heuristic: the HTML Standard's encoding sniffing
/// algorithm consults it FIRST, ahead of the transport charset and ahead of any `<meta charset>`. In
/// WPT core this is FIVE source files reaching no verdict across 30 measured URLs:
/// `html/infrastructure/urls/resolving-urls/query-encoding/utf-16{be,le}.html` (27 of their
/// `?include=` variants), `html/syntax/charset/without-inheritance.html`, and
/// `html/syntax/parsing-html-fragments/the-input-byte-stream-00{3,4}.html`. A body with no BOM keeps
/// exactly the previous UTF-8-lossy behaviour, so nothing else in the corpus changes.
fn decode_document(bytes: &[u8]) -> String {
    match bytes {
        [0xFE, 0xFF, rest @ ..] => decode_utf16(rest, true),
        [0xFF, 0xFE, rest @ ..] => decode_utf16(rest, false),
        // A UTF-8 BOM is not part of the document; leaving it in puts U+FEFF before `<!doctype`.
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

fn decode_utf16(bytes: &[u8], big_endian: bool) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| if big_endian { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) })
        .collect();
    String::from_utf16_lossy(&units)
}

/// Does this document carry `<meta name="timeout" content="long">`?
///
/// testharness.js reads exactly this meta (`WindowTestEnvironment.test_timeout`) to choose a 60s
/// harness timeout over the 10s default, and wptrunner scales its own per-test budget the same way. We
/// were the only party in the loop ignoring it, which is why `scripting-1/moving-between-documents`
/// (every file there carries it) and the `xhr/xmlhttprequest-timeout-*` family were cut off mid-run.
///
/// Deliberately a scan of the served document rather than of the source file: for a `.any.js` the meta
/// comes from wptserve's generated wrapper (compiled from `// META: timeout=long`), and the wrapper is
/// what we actually load. Only the head of the document is scanned -- metas live in `<head>` and some
/// WPT files are large.
fn wants_long_timeout(html: &str) -> bool {
    let head: String = html.chars().take(8000).collect::<String>().to_ascii_lowercase();
    let mut from = 0usize;
    while let Some(i) = head[from..].find("<meta") {
        let start = from + i;
        let end = head[start..].find('>').map(|j| start + j).unwrap_or(head.len());
        let tag = &head[start..end];
        // Both `name=timeout` and `name="timeout"` occur in the corpus, so accept either quoting.
        let attr_is = |attr: &str, val: &str| {
            tag.find(attr).is_some_and(|k| {
                let rest = tag[k + attr.len()..].trim_start();
                let rest = rest.strip_prefix('=').unwrap_or(rest).trim_start();
                rest.trim_start_matches(['"', '\'']).starts_with(val)
            })
        };
        if attr_is("name", "timeout") && attr_is("content", "long") {
            return true;
        }
        from = end.max(start + 5);
    }
    false
}

/// The three helpers above are the ones whose failure is SILENT: each one breaks by producing a file
/// that reaches no verdict, and a no-verdict file is 0/0 -- it leaves the denominator instead of
/// lowering the pass rate, so the scoreboard looks the same or better while measuring less. That is
/// precisely the failure mode this corner of the runner exists to prevent, so it is asserted rather
/// than trusted. Cheap and non-ignored: pure functions, no engine, no server.
#[cfg(test)]
mod harness_helpers {
    use super::{decode_document, encode_variant, wants_long_timeout};

    #[test]
    fn variant_space_is_encoded_but_the_query_survives() {
        // The 30-file bucket: a raw space here made the request line four tokens and wptserve
        // answered 400.
        assert_eq!(encode_variant("?load fires normally"), "?load%20fires%20normally");
        assert_eq!(
            encode_variant("?timeout set to non-expiring value after timeout fires"),
            "?timeout%20set%20to%20non-expiring%20value%20after%20timeout%20fires"
        );
        // Query punctuation must NOT be touched, or every `?include=`-style variant addresses a
        // different URL than the one the manifest named.
        assert_eq!(encode_variant("?include=xhr"), "?include=xhr");
        assert_eq!(encode_variant("?1-4&stop=2"), "?1-4&stop=2");
        assert_eq!(encode_variant(""), "");
        // The test reads its sub-case back with decodeURIComponent, so %20 is what must arrive.
        assert_eq!(encode_variant("?a\"b#c<d>e"), "?a%22b%23c%3Cd%3Ee");
    }

    #[test]
    fn utf16_documents_decode_instead_of_shredding() {
        let mut be: Vec<u8> = vec![0xFE, 0xFF];
        for u in "<script>".encode_utf16() {
            be.extend_from_slice(&u.to_be_bytes());
        }
        assert_eq!(decode_document(&be), "<script>");

        let mut le: Vec<u8> = vec![0xFF, 0xFE];
        for u in "<script>".encode_utf16() {
            le.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(decode_document(&le), "<script>");

        // A UTF-8 BOM is stripped; a plain UTF-8 body is untouched (the whole rest of the corpus).
        assert_eq!(decode_document(b"\xEF\xBB\xBF<!doctype html>"), "<!doctype html>");
        assert_eq!(decode_document(b"<!doctype html>"), "<!doctype html>");
    }

    #[test]
    fn meta_timeout_long_is_recognised_in_the_forms_wpt_writes() {
        // Bare attributes, as xhr/xmlhttprequest-timeout-simple.html writes it.
        assert!(wants_long_timeout("<meta charset=utf-8>\n<meta name=timeout content=long>"));
        // Quoted, as the generated `.any.html` wrappers write it.
        assert!(wants_long_timeout(r#"<meta name="timeout" content="long">"#));
        // A file with no such meta, and one whose OTHER metas must not be mistaken for it -- a
        // false positive costs 3x the budget on a file that never needed it.
        assert!(!wants_long_timeout("<meta charset=utf-8><title>x</title>"));
        assert!(!wants_long_timeout(r#"<meta name="variant" content="?long tail">"#));
        assert!(!wants_long_timeout("<meta name=timeout content=normal>"));
    }
}

/// THE RATCHET'S UNMEASURED BUCKET, PROVED TO GO RED.
///
/// `scripts/wpt-tiers.sh` no longer treats a file the watchdog terminated as lost coverage: it is out
/// of the judgement and counted in its place, read from the `hit_watchdog` this file writes per run.
/// That is worth exactly as much as the proof that it still goes red for a real failure, and this tree
/// has shipped two gates that could not. Four cases, two per half of the ratchet, each pair differing
/// in ONE field:
///
///   coverage half   a floor area's only file reaches no verdict ... watchdog silent -> RED, by name
///                                                              ... watchdog fired  -> green, counted
///   conformance     a floor `pass` file loses a subtest         ... watchdog silent -> RED, by name
///                                                              ... watchdog fired  -> green, EXCUSED
///
/// It drives the REAL script through `KORVUS_WPT_RESCORE` (the mechanism its own header documents for
/// this purpose) over a synthetic shard JSON, so no WPT checkout, no engine and no sweep is involved
/// and it costs the gate well under a second. `KORVUS_WPT_ROOT` points at an empty scaffold whose only
/// job is to hold a `encoding/` directory, because the script refuses to score a root that has none of
/// the tier's subtrees -- deliberately, since a missing checkout must never read as a regression.
#[cfg(test)]
mod ratchet_bucket {
    use std::path::PathBuf;

    /// `<tier> pass` / `<tier> verdicts` rows for four files, each its own `<area>` -- which is the
    /// real shape: 446 of the live floor's 506 areas are a single top-level file, and that is why one
    /// terminated file used to fail the coverage half all by itself.
    const FLOOR: &str = "\
# GENERATED by scripts/wpt-tiers.sh --write-baseline [tier] -- synthetic floor for this test.
core verdicts encoding/fails.html 1
core verdicts encoding/killed.html 1
core verdicts encoding/regress.html 1
core verdicts encoding/terminated.html 1
core pass encoding/fails.html
core pass encoding/terminated.html
";

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    /// One scored run. `files` is `(path, passed, total, hit_watchdog)`; everything else is the
    /// runner's own JSON shape. Returns (exit code, combined output).
    fn score(case: &str, files: &[(&str, usize, usize, bool)]) -> (i32, String) {
        score_with_recheck(case, files, None)
    }

    /// [`score`], with a stand-in for the runner the SOLO RE-CHECK launches.
    ///
    /// `recheck` is the body of a shell script put at `$BIN`. The scorer runs it with
    /// `KORVUS_WPT_LIST` and `KORVUS_WPT_OUT` set, exactly as it runs the real test binary, so a
    /// stub that writes a `files` array decides what "passes alone" means without launching an
    /// engine. `None` leaves `$BIN` unset, which is the RESCORE shape: nothing to re-check with.
    fn score_with_recheck(
        case: &str,
        files: &[(&str, usize, usize, bool)],
        recheck: Option<&str>,
    ) -> (i32, String) {
        score_full(case, files, recheck, false).0
    }

    /// [`score_with_recheck`], and optionally in `--write-baseline` mode, returning the floor's text
    /// alongside the run's so a test can assert what was actually RECORDED rather than what was said.
    fn score_full(
        case: &str,
        files: &[(&str, usize, usize, bool)],
        recheck: Option<&str>,
        write_baseline: bool,
    ) -> ((i32, String), String) {
        let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("wpt-ratchet").join(case);
        let (work, root) = (base.join("work"), base.join("root"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&work).expect("work dir");
        std::fs::create_dir_all(root.join("encoding")).expect("root scaffold");
        let floor = base.join("floor");
        std::fs::write(&floor, FLOOR).expect("write floor");

        let rows: Vec<serde_json::Value> = files
            .iter()
            .map(|(p, passed, total, wd)| {
                serde_json::json!({
                    "path": p, "area": p.split('/').next().unwrap(),
                    "passed": passed, "total": total,
                    // 0 = OK, 1 = ERROR. Never 2 (testharness TIMEOUT), which the ratchet excludes
                    // from `passing` on its own and would confound the one field under test.
                    "harness_status": if *total > 0 && passed == total { 0 } else { 1 },
                    "errors": [], "completed": true, "harness_loaded": true,
                    "tests_registered": total, "hit_pump_budget": false,
                    "hit_watchdog": wd, "watchdog_terminations": if *wd { 1 } else { 0 },
                    "failures": [], "subtests": [],
                })
            })
            .collect();
        std::fs::write(
            work.join("shard-0.json"),
            serde_json::to_string(&serde_json::json!({
                "harness": "synthetic", "overall_passed": 0, "overall_total": 0,
                "files_count": rows.len(), "areas": [], "files": rows,
            }))
            .unwrap(),
        )
        .expect("write shard JSON");

        let mut cmd = std::process::Command::new("bash");
        cmd.arg(repo_root().join("scripts/wpt-tiers.sh"));
        if write_baseline {
            cmd.arg("--write-baseline");
        }
        cmd.args(["core", "1"])
            .env("KORVUS_WPT_ROOT", &root)
            .env("KORVUS_WPT_WORK", &work)
            .env("KORVUS_WPT_RESCORE", "1")
            .env("KORVUS_WPT_BASELINE", &floor);
        if let Some(body) = recheck {
            let stub = base.join("recheck.sh");
            std::fs::write(&stub, format!("#!/usr/bin/env bash\n{body}\n")).expect("write stub");
            let mut perm = std::fs::metadata(&stub).expect("stat stub").permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
            std::fs::set_permissions(&stub, perm).expect("chmod stub");
            cmd.env("BIN", &stub);
        }
        let out = cmd.output().expect("run scripts/wpt-tiers.sh");
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        let written = std::fs::read_to_string(&floor).unwrap_or_default();
        ((out.status.code().unwrap_or(-1), text), written)
    }

    /// The four files as they are when nothing is wrong. Each case below changes ONE of them.
    const CLEAN: [(&str, usize, usize, bool); 4] = [
        ("encoding/fails.html", 10, 10, false),
        ("encoding/terminated.html", 7, 7, false),
        ("encoding/regress.html", 3, 3, false),
        ("encoding/killed.html", 5, 5, false),
    ];

    fn with(i: usize, row: (&'static str, usize, usize, bool)) -> [(&'static str, usize, usize, bool); 4] {
        let mut rows = CLEAN;
        rows[i] = row;
        rows
    }

    #[test]
    fn a_clean_run_is_green() {
        let (rc, out) = score("clean", &CLEAN);
        assert_eq!(rc, 0, "a run matching the floor must pass:\n{out}");
        assert!(out.contains("wpt-tiers: OK"), "{out}");
        assert!(
            out.contains("the watchdog terminated 0 of 4 file(s)"),
            "the unmeasured count is printed on EVERY run, including a green one -- a bucket nobody \
             can see is a bucket that grows:\n{out}"
        );
    }

    #[test]
    fn a_pass_line_that_really_fails_still_goes_red_by_name() {
        let (rc, out) = score("lost", &with(0, ("encoding/fails.html", 9, 10, false)));
        assert_eq!(rc, 1, "a file that stops passing must fail the gate:\n{out}");
        assert!(out.contains("wpt-tiers: REGRESSED"), "{out}");
        assert!(
            out.contains("encoding/fails.html"),
            "the failure must NAME the file, not just report a number:\n{out}"
        );
    }

    /// A file that fails the eleven-shard tier and PASSES alone is contention, and the gate stays
    /// green with the file named.
    ///
    /// The case this was built from: `html/webappapis/timers/timer-nesting-not-inherited-in-microtask.html`
    /// failed the tier twice at `assert_less_than: ... expected a number less than 4 but got 4.5`,
    /// then passed 2/2 when shard 3 was re-run ALONE -- same shard, same position 481/526, same
    /// neighbours. Before the re-check the only sanctioned answer was `--write-baseline`, whose
    /// intersection DELETES the line from the floor for good.
    #[test]
    fn a_candidate_that_passes_alone_is_contention_not_a_regression() {
        // The stub answers as list mode does: the url path, WITH the leading slash the floor does
        // not carry. Comparing those two raw is what made the first version of this report every
        // re-checked file as still failing.
        let (rc, out) = score_with_recheck(
            "contention",
            &with(0, ("encoding/fails.html", 9, 10, false)),
            Some(
                r#"printf '{"files":[{"path":"/encoding/fails.html","passed":10,"total":10,"harness_status":0}]}' > "$KORVUS_WPT_OUT""#,
            ),
        );
        assert_eq!(rc, 0, "a file that passes in a quiet process is passing:\n{out}");
        assert!(out.contains("PASSED when re-run alone"), "{out}");
        assert!(
            out.contains("encoding/fails.html"),
            "contention is not the same as invisible -- the file must be named:\n{out}"
        );
        assert!(
            !out.contains("wpt-tiers: REGRESSED"),
            "the solo pass must clear the regression, not sit beside it:\n{out}"
        );
    }

    /// `--write-baseline` must NOT drop a recorded line that only lost the eleven-way race.
    ///
    /// This is where the floor is actually destroyed: the intersection deletes any recorded `pass`
    /// that did not pass THIS run, permanently, from the only record that the file ever passed. On
    /// 2026-09-18 a re-record started under a load of 14 and was killed by hand seconds before it
    /// would have taken a line with it -- which is a procedure, not a fix, because it depends on
    /// whoever types the command knowing what else is on the box.
    #[test]
    fn writing_the_baseline_keeps_a_line_that_passes_alone() {
        let ((rc, out), floor) = score_full(
            "bank-contention",
            &with(0, ("encoding/fails.html", 9, 10, false)),
            Some(r#"printf '{"files":[{"path":"/encoding/fails.html","passed":10,"total":10,"harness_status":0}]}' > "$KORVUS_WPT_OUT""#),
            true,
        );
        assert_eq!(rc, 0, "writing a floor must succeed here:\n{out}");
        assert!(out.contains("passed alone"), "{out}");
        assert!(
            floor.contains("core pass encoding/fails.html"),
            "the line passed alone, so it must STILL BE IN THE FLOOR:\n{floor}"
        );
    }

    /// ... and a line that fails alone too is dropped, which is the intersection doing its job.
    #[test]
    fn writing_the_baseline_drops_a_line_that_fails_alone_too() {
        let ((rc, out), floor) = score_full(
            "bank-realdrop",
            &with(0, ("encoding/fails.html", 9, 10, false)),
            Some(r#"printf '{"files":[{"path":"/encoding/fails.html","passed":9,"total":10,"harness_status":1}]}' > "$KORVUS_WPT_OUT""#),
            true,
        );
        assert_eq!(rc, 0, "{out}");
        assert!(
            !floor.contains("core pass encoding/fails.html"),
            "a line that fails in both conditions is not a floor:\n{floor}"
        );
    }

    /// A re-record that CANNOT re-check refuses to write, rather than dropping lines it could not judge.
    #[test]
    fn writing_the_baseline_refuses_when_it_cannot_recheck() {
        let ((rc, out), floor) = score_full(
            "bank-unproven",
            &with(0, ("encoding/fails.html", 9, 10, false)),
            Some("exit 0"),                        // writes no KORVUS_WPT_OUT
            true,
        );
        assert_eq!(rc, 1, "an unjudgeable drop must not be written:\n{out}");
        assert!(out.contains("REFUSING to write a floor"), "{out}");
        assert!(
            floor.contains("core pass encoding/fails.html"),
            "the floor must be untouched when the writer refuses:\n{floor}"
        );
    }

    /// A file that fails the tier AND fails alone is a regression exactly as before.
    #[test]
    fn a_candidate_that_fails_alone_too_is_still_a_regression() {
        let (rc, out) = score_with_recheck(
            "still-red",
            &with(0, ("encoding/fails.html", 9, 10, false)),
            Some(r#"printf '{"files":[{"path":"/encoding/fails.html","passed":9,"total":10,"harness_status":1}]}' > "$KORVUS_WPT_OUT""#),
        );
        assert_eq!(rc, 1, "the re-check must not become a way to pass:\n{out}");
        assert!(out.contains("wpt-tiers: REGRESSED"), "{out}");
        assert!(out.contains("FAILED AGAIN when re-run alone"), "{out}");
    }

    /// A re-check that writes NOTHING leaves the candidate red -- it judged nothing, and "could not
    /// tell" must never read as "passed".
    ///
    /// Found by pointing `$BIN` at `/bin/true`: the scorer parsed the PREVIOUS re-check's
    /// `solo.json`, still sitting in a persistent `KORVUS_WPT_WORK`, and went green on a stale pass.
    #[test]
    fn a_recheck_that_answers_nothing_leaves_the_candidate_red() {
        let (rc, out) = score_with_recheck(
            "unproven",
            &with(0, ("encoding/fails.html", 9, 10, false)),
            Some("exit 0"),                       // writes no KORVUS_WPT_OUT at all
        );
        assert_eq!(rc, 1, "an unproven candidate must stay red:\n{out}");
        assert!(out.contains("wpt-tiers: REGRESSED"), "{out}");
        assert!(out.contains("NOT re-checked"), "the report must say it could not judge:\n{out}");
        assert!(
            !out.contains("FAILED AGAIN when re-run alone"),
            "it did not fail alone -- it was never asked:\n{out}"
        );
    }

    #[test]
    fn a_pass_line_the_watchdog_terminated_is_excused_and_named() {
        // Byte-for-byte the previous case with `hit_watchdog` flipped. Nothing else moves.
        let (rc, out) = score("excused", &with(0, ("encoding/fails.html", 9, 10, true)));
        assert_eq!(
            rc, 0,
            "a file the harness could not measure is not a conformance regression:\n{out}"
        );
        assert!(out.contains("EXCUSED, not judged"), "{out}");
        assert!(
            out.contains("encoding/fails.html"),
            "excused is not the same as invisible -- the file must still be named:\n{out}"
        );
    }

    #[test]
    fn coverage_that_falls_without_the_watchdog_still_goes_red_by_name() {
        let (rc, out) = score("fell", &with(2, ("encoding/regress.html", 0, 0, false)));
        assert_eq!(rc, 1, "a file that stops reaching a verdict must fail the gate:\n{out}");
        assert!(out.contains("REGRESSED (coverage)"), "{out}");
        assert!(out.contains("encoding/regress.html"), "{out}");
    }

    #[test]
    fn coverage_the_watchdog_took_is_counted_not_lost() {
        // Byte-for-byte the previous case with `hit_watchdog` flipped.
        let (rc, out) = score("counted", &with(2, ("encoding/regress.html", 0, 0, true)));
        assert_eq!(rc, 0, "a terminated file must not read as lost coverage:\n{out}");
        assert!(out.contains("wpt-tiers: OK"), "{out}");
        assert!(
            out.contains("1 of them before any subtest settled"),
            "the no-verdict half of the bucket is reported separately, because only it can move an \
             area's count:\n{out}"
        );
    }
}

/// The corpus root. Defaults to the small vendored fixture; `KORVUS_WPT_ROOT` points it at a full
/// upstream checkout instead (`git clone --depth=1 .../wpt reference/wpt`), which is how the corpus
/// scales from the vendored handful to the ~26k runnable upstream files without putting 1.1 GB in git.
///
/// The upstream tree needs `resources/testharness.js` at the root, which a real checkout has — so the
/// same static server + report-hook path works unchanged. Korvus's own report hook must still win over
/// upstream's: [`serve`] rewrites `/resources/testharnessreport.js` to the vendored hook.
fn wpt_root() -> PathBuf {
    match std::env::var("KORVUS_WPT_ROOT") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => vendored_wpt_root(),
    }
}

/// The in-repo fixture root — also where the Korvus report hook lives when `KORVUS_WPT_ROOT` points
/// somewhere else.
fn vendored_wpt_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/wpt")
}

/// Subtrees to walk, from `KORVUS_WPT_DIRS` (comma-separated, e.g. `dom,url,encoding`). Empty ⇒ the
/// whole root. Pointing at a full upstream checkout without this would enumerate ~26k files; the
/// campaign enables areas one at a time, in queue order.
fn wpt_dirs() -> Vec<String> {
    std::env::var("KORVUS_WPT_DIRS")
        .ok()
        .map(|s| s.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect())
        .unwrap_or_default()
}

/// Content-Type by file extension -- enough for a static test corpus (html/js/json + a couple extras).
fn content_type(path: &str) -> &'static str {
    let p = path.to_ascii_lowercase();
    if p.ends_with(".html") || p.ends_with(".xhtml") || p.ends_with(".htm") {
        "text/html; charset=utf-8"
    } else if p.ends_with(".js") || p.ends_with(".mjs") {
        "text/javascript; charset=utf-8"
    } else if p.ends_with(".json") {
        "application/json; charset=utf-8"
    } else if p.ends_with(".svg") {
        "image/svg+xml"
    } else if p.ends_with(".txt") {
        "text/plain; charset=utf-8"
    } else if p.ends_with(".css") {
        // wptserve's answer (Python's `mimetypes`). Served as `application/octet-stream`, every external
        // sheet in the corpus was refused by the engine's MIME check -- which it had to be -- and once a
        // refused sheet fired `error` rather than `load` the difference showed
        // (`the-link-element/link-load-event-alt-001.html`, whose `style.css` must load).
        "text/css"
    } else {
        "application/octet-stream"
    }
}

/// A path-routing HTTP/1.1 static file server (the raw-`TcpListener` pattern from tests/subresource.rs,
/// upgraded from single-body to path routing). Reads the request line, maps `GET /a/b.js` -> file
/// `<root>/a/b.js`, serves it with a correct Content-Type, 404s otherwise. One request per connection
/// (Connection: close), each handled on its own task so `load_page`'s concurrent subresource fetches
/// don't serialize. Traversal (`..`) is refused. Bind 127.0.0.1:0; caller supplies the OS-chosen port.
async fn serve(listener: TcpListener, root: PathBuf) {
    loop {
        let (mut s, _) = match listener.accept().await {
            Ok(v) => v,
            Err(_) => return,
        };
        let root = root.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            let n = match s.read(&mut buf).await {
                Ok(n) => n,
                Err(_) => return,
            };
            let head = String::from_utf8_lossy(&buf[..n]);
            let line = head.lines().next().unwrap_or("");
            let mut parts = line.split_whitespace();
            let method = parts.next().unwrap_or("");
            let target = parts.next().unwrap_or("/");
            let path = target.split('?').next().unwrap_or("/");
            let rel = path.trim_start_matches('/');
            // wptserve's REWRITES table, transcribed (`tools/serve/serve.py`):
            //     rewrites = [("GET", "/resources/WebIDLParser.js",
            //                         "/resources/webidl2/lib/webidl2.js")]
            // `/resources/WebIDLParser.js` IS NOT A FILE IN WPT and never was -- it is a server
            // route, and `tools/wpt/testfiles.py` carries the inverse mapping. Two independently
            // obtained clones a month apart both lack the path, and upstream returns 404 for it at
            // the pinned rev, so its absence from the corpus is FAITHFUL; what was missing was this
            // rule. Serving the 404 cost 5 `html/webappapis` files their verdict outright, and left
            // 12 more scoring `0/1` -- one failing "idl_test setup" subtest from
            // `ReferenceError: WebIDL2 is not defined` -- which the floor counted as ACCOUNTED FOR
            // while they measured nothing. Restoring it: 13 -> 18 verdicts, 13 -> 3,718 declared
            // subtests, 0 -> 3,505 passing, over the 17 core files in scope (W2, measured).
            let rel = if method == "GET" && rel == "resources/WebIDLParser.js" {
                "resources/webidl2/lib/webidl2.js"
            } else {
                rel
            };

            // `xhr/resources/delay.py`, the ONE handler this static server emulates: sleep
            // `ms` (default 500), then answer `TEST_DELAY` as text/plain -- the file's own four lines.
            // Served as a static path it was an immediate 404, which turned every "a 5 ms timeout
            // fires before a 20 s response" test into a RACE between the timer and the 404:
            // `xhr/event-timeout-order.any.js` passed or failed run to run on one binary (the
            // coordinator's three runs: fail, pass, fail). google-chrome 150 against a server that
            // really delays 20 ms with `timeout = 5` answers `upload.timeout`, never `upload.progress`;
            // against a 0 ms server it answers `upload.progress` at 1-3 ms, as this engine did. The
            // engine was not racing wrongly -- the server was not delaying. The redirect handlers
            // below joined it in L20, when a measured need appeared; every other `.py` still 404s.
            let delay_ms = (rel == "xhr/resources/delay.py").then(|| {
                target
                    .split_once('?')
                    .and_then(|(_, q)| q.split('&').find_map(|kv| kv.strip_prefix("ms=")))
                    .and_then(|v| v.parse::<f64>().ok())
                    .unwrap_or(500.0)
            });
            if let Some(ms) = delay_ms {
                tokio::time::sleep(Duration::from_micros((ms.max(0.0) * 1000.0) as u64)).await;
                let body = b"TEST_DELAY";
                let resp_head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: YO\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(resp_head.as_bytes()).await;
                let _ = s.write_all(body).await;
                let _ = s.flush().await;
                return;
            }
            // The three redirect handlers -- see `redirect_handler`.
            if let Some((resp, delay)) = redirect_handler(rel, &WptRequest::parse(&head)) {
                if let Some(d) = delay {
                    tokio::time::sleep(d).await;
                }
                resp.write_to(&mut s).await;
                return;
            }
            let resp = if rel.contains("..") {
                WptResponse::plain(403, b"forbidden".to_vec())
            } else {
                // The report hook is ALWAYS Korvus's, never the corpus's. Upstream ships a
                // `testharnessreport.js` that POSTs results back to wptserve; ours captures them
                // in-process onto `globalThis.__wptresults`. Overriding by path keeps a full upstream
                // checkout (`KORVUS_WPT_ROOT`) working with no edits to the checkout itself.
                let hook = rel == "resources/testharnessreport.js";
                let fp = if hook { vendored_wpt_root().join(rel) } else { root.join(rel) };
                if fp.is_file() {
                    match std::fs::read(&fp) {
                        // The report hook is ours and is served as it stands; every corpus file goes
                        // through wptserve's FileHandler model -- `.headers` sidecars, then the
                        // `.sub.` / `?pipe=` pipeline. See `static_file_response`.
                        Ok(bytes) if hook => WptResponse::file(content_type(path), bytes),
                        Ok(bytes) => static_file_response(&root, rel, &WptRequest::parse(&head), bytes),
                        Err(_) => WptResponse::plain(404, b"read error".to_vec()),
                    }
                } else {
                    WptResponse::plain(404, format!("not found: {path}").into_bytes())
                }
            };
            resp.write_to(&mut s).await;
        });
    }
}

// ─── wptserve's FileHandler, the parts that decide outcomes ───────────────────────────
//
// THE STATIC SERVER USED TO IGNORE `?pipe=`, AND 37 SCRIPT-ORDERING TESTS WERE RACES BECAUSE OF IT.
// `html/semantics/scripting-1/the-script-element/execution-timing/` requests
// `scripts/include-1.js?pipe=trickle(d1)` in 37 of its files: the test's expected order depends on
// one script arriving SECONDS after another. Served instantly, which one ran first was decided by
// thread scheduling, and the floor held whichever outcome the recording runs happened to see -- so
// `execution-timing/{050,078,091,105,123}.html` went red on good trees, a different one each run,
// only under load (P10d: 2 of 11 parallel runs).
//
// What is emulated, and why each is wptserve's behaviour rather than a guess (tools/wptserve/wptserve/):
//   * `load_headers` (handlers.py): `__dir__.headers` then `<file>.headers` for EVERY static file, not
//     only the test document; a sidecar's Content-Type replaces the guessed one.
//   * `wrap_pipeline` (handlers.py): the pipe string is `sub(...)` when the path has `.sub.`, then the
//     LAST `pipe=` query value, joined with `|`, applied left to right.
//   * `status`, `header`, `trickle`, `slice` (pipes.py), with their argument grammar and PipeTokenizer's
//     escapes. `trickle` sends the headers at once and the body in timed parts with no Content-Length
//     (wptserve writes Content-Length only for a bytes body), and adds the no-cache trio when the
//     response has no cache header of its own -- the cache is part of what makes the order observable.
//   * `sub` (pipes.py `template`): see `wpt_substitute` for the subset and why that subset.
// An unknown pipe or a malformed argument makes wptserve answer 500; so does this.

/// The parts of a request wptserve's pipes read: the request target and the request headers.
struct WptRequest {
    method: String,
    /// Path plus query, as on the request line.
    target: String,
    headers: Vec<(String, String)>,
}

impl WptRequest {
    fn parse(head: &str) -> Self {
        let mut lines = head.split("\r\n");
        let first = lines.next().unwrap_or("");
        let method = first.split_whitespace().next().unwrap_or("GET").to_string();
        let target = first.split_whitespace().nth(1).unwrap_or("/").to_string();
        let headers = lines
            .take_while(|l| !l.is_empty())
            .filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
            .collect();
        WptRequest { method, target, headers }
    }

    /// The request a document read off disk WOULD have been fetched with: `base` is its URL.
    fn for_url(base: &str) -> Self {
        let rest = base.split_once("://").map(|(_, r)| r).unwrap_or(base);
        let (hostport, target) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        WptRequest { method: "GET".into(), target: target.to_string(), headers: vec![("Host".into(), hostport.into())] }
    }

    fn path(&self) -> &str {
        self.target.split(['?', '#']).next().unwrap_or("/")
    }

    fn query(&self) -> &str {
        let q = self.target.split_once('?').map(|(_, q)| q).unwrap_or("");
        q.split('#').next().unwrap_or("")
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    /// (hostname, port) from the Host header.
    fn host_port(&self) -> (String, String) {
        let h = self.header("host").unwrap_or("127.0.0.1:80");
        match h.rsplit_once(':') {
            Some((host, port)) => (host.to_string(), port.to_string()),
            None => (h.to_string(), "80".to_string()),
        }
    }
}

/// wptserve's `template` (pipes.py): `{{...}}` placeholders, tokenised as `$var:` / `ident` / `[index]`
/// / `(args)`, escaped for HTML (`&<>"'`) when `html`.
///
/// ONE ORIGIN, SO A SUBSET -- and what is outside it is LEFT VERBATIM, not answered 500. This server
/// is one HTTP origin on 127.0.0.1. What that origin can truthfully answer is substituted: `host`,
/// `ports[http][0]`, `location[...]`, `GET[...]`, `headers[...]`, `header_or_default(...)`, `uuid()`,
/// `url_base`, `file_hash(sha*, path)` and `$variables` -- and `ports[ws|wss|h2][0]`, whose servers
/// the runner DOES start (`ws_ports`: WPT's own pywebsocket3, as `wpt serve` runs it), along with the
/// host names of a WebSocket URL on them. The rest -- `domains[www]`, `hosts[alt][]`,
/// `ports[https][0]`, `ports[http][1]` -- names ANOTHER origin this server does not have;
/// wptserve would substitute a host that resolves, and inventing one that does not (or mapping it to
/// this same origin, which would make every cross-origin test same-origin) is worse than leaving the
/// placeholder visibly unfilled. wptserve answers 500 only for a name it does not know at all; that
/// case stays verbatim too, because a static file it would 500 on is not one this corpus ships.
fn wpt_substitute(content: &[u8], req: &WptRequest, html: bool) -> Vec<u8> {
    let text = String::from_utf8_lossy(content);
    if !text.contains("{{") {
        return content.to_vec();
    }
    let mut vars: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut out = String::with_capacity(text.len());
    let mut rest: &str = &text;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        // `{{([^}]*)}}`: the body holds no `}`, and must be followed by `}}` right away.
        let body_len = after.find('}').unwrap_or(after.len());
        if !after[body_len..].starts_with("}}") {
            out.push_str("{{");
            rest = after;
            continue;
        }
        let body = &after[..body_len];
        // A HOST NAME IN A WEBSOCKET URL is the one cross-origin name this runner can answer
        // truthfully: WPT's own WebSocket servers (`ws_ports`) answer every host on their port, as
        // `wpt serve`'s do, so `ws://{{domains[www1]}}:{{ports[ws][0]}}/echo` names an endpoint that
        // exists. Everywhere else `domains[]` / `hosts[][]` stay verbatim (the reasoning above).
        let next = &after[body_len + 2..];
        let ws_url_host = next.starts_with(":{{ports[ws][0]}}") || next.starts_with(":{{ports[wss][0]}}");
        let value = if ws_url_host { ws_host(body, req).or_else(|| substitute_one(body, req, &mut vars)) } else { substitute_one(body, req, &mut vars) };
        match value {
            Some(v) => {
                if html {
                    for c in v.chars() {
                        match c {
                            '&' => out.push_str("&amp;"),
                            '<' => out.push_str("&lt;"),
                            '>' => out.push_str("&gt;"),
                            '"' => out.push_str("&quot;"),
                            '\'' => out.push_str("&#x27;"),
                            c => out.push(c),
                        }
                    }
                } else {
                    out.push_str(&v);
                }
            }
            None => {
                out.push_str("{{");
                out.push_str(body);
                out.push_str("}}");
            }
        }
        rest = &after[body_len + 2..];
    }
    out.push_str(rest);
    out.into_bytes()
}

// ─── WPT's WebSocket servers ──────────────────────────────────────────────────────────────────────
//
// `wpt serve` starts pywebsocket3 on `ports[ws]` and `ports[wss]` with `websockets/handlers/*_wsh.py`
// (tools/serve/serve.py `WebSocketDaemon`). Until korvus had a WebSocket the runner needed none: the
// stub accepted any URL, so `new WebSocket('ws://web-platform.test:{{ports[ws][0]}}/echo')` "passed"
// 78 files with the placeholder unfilled. With a real WebSocket that URL is a SyntaxError -- in Chrome
// too -- so the servers are started here and their ports substituted, and the files pass or fail on
// what the socket does.
//
// ONCE PER RUN. `scripts/wpt-tiers.sh` starts `scripts/wpt-ws-servers.py` once for all its shards and
// hands the ports over in `KORVUS_WPT_WS_PORTS=ws=P,wss=P,h2=P`; a test binary run directly starts its
// own, which lives exactly as long as this process (its stdin is this process's pipe). Without a WPT
// checkout that carries pywebsocket3 (the vendored fixture does not) there are no servers and the
// placeholders stay verbatim, as before.
//
// `h2` is held, not served: `?wpt_flags=h2` is WebSocket over HTTP/2 (RFC 8441), which korvus does not
// do (see the launcher's header). `wss` is WPT's `web-platform.test` certificate, trusted through
// `KORVUS_EXTRA_CA=tools/certs/cacert.pem` -- what wptrunner does for the browser it runs.

/// The three ports of the run's WebSocket servers.
struct WsPorts {
    ws: u16,
    wss: u16,
    h2: u16,
}

static WS_PORTS: std::sync::OnceLock<Option<WsPorts>> = std::sync::OnceLock::new();
/// The launcher this process started, if it started one: held so its stdin stays open (closing it is
/// what stops the servers), never waited on.
static WS_LAUNCHER: std::sync::Mutex<Option<std::process::Child>> = std::sync::Mutex::new(None);

fn ws_ports() -> Option<&'static WsPorts> {
    WS_PORTS.get().and_then(|p| p.as_ref())
}

/// `ws=P,wss=P,h2=P` (any order); `None` unless all three are there.
fn parse_ws_ports(s: &str) -> Option<WsPorts> {
    let mut m = std::collections::HashMap::new();
    for kv in s.split(',') {
        let (k, v) = kv.split_once('=')?;
        m.insert(k.trim().to_string(), v.trim().parse::<u16>().ok()?);
    }
    Some(WsPorts { ws: *m.get("ws")?, wss: *m.get("wss")?, h2: *m.get("h2")? })
}

/// Start (or adopt) the run's WebSocket servers, once. Also points `KORVUS_EXTRA_CA` at WPT's CA when
/// nothing else did, before any transport client exists, so `wss` verifies.
fn init_ws_servers(root: &Path) {
    WS_PORTS.get_or_init(|| {
        let ca = root.join("tools/certs/cacert.pem");
        let ports = if let Ok(v) = std::env::var("KORVUS_WPT_WS_PORTS") {
            let p = parse_ws_ports(&v);
            if p.is_none() {
                eprintln!("wpt: KORVUS_WPT_WS_PORTS={v:?} is not `ws=P,wss=P,h2=P` -- no WebSocket servers");
            }
            p
        } else {
            spawn_ws_servers(root)
        };
        if ports.is_some() && std::env::var_os("KORVUS_EXTRA_CA").is_none() && ca.is_file() {
            std::env::set_var("KORVUS_EXTRA_CA", &ca);
        }
        match &ports {
            Some(p) => eprintln!("wpt: WebSocket servers ws={} wss={} h2(held)={}", p.ws, p.wss, p.h2),
            None => eprintln!("wpt: no WebSocket servers -- `{{{{ports[ws|wss|h2][0]}}}}` stay unfilled"),
        }
        ports
    });
}

fn spawn_ws_servers(root: &Path) -> Option<WsPorts> {
    if !root.join("tools/third_party/pywebsocket3").is_dir() || !root.join("websockets/handlers").is_dir() {
        return None;
    }
    let launcher = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/wpt-ws-servers.py");
    let port_file = std::env::temp_dir().join(format!(
        "korvus-wpt-ws-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
    ));
    let child = std::process::Command::new("python3")
        .arg("-I")
        .arg(&launcher)
        .arg(root)
        .arg(&port_file)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let child = match child {
        Ok(c) => c,
        Err(e) => {
            eprintln!("wpt: cannot start {}: {e}", launcher.display());
            return None;
        }
    };
    *WS_LAUNCHER.lock().unwrap() = Some(child);
    let t0 = std::time::Instant::now();
    while t0.elapsed() < Duration::from_secs(20) {
        if let Ok(s) = std::fs::read_to_string(&port_file) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                let _ = std::fs::remove_file(&port_file);
                let p = |k: &str| v[k].as_u64().map(|n| n as u16);
                return Some(WsPorts { ws: p("ws")?, wss: p("wss")?, h2: p("h2")? });
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    eprintln!("wpt: {} never reported its ports", launcher.display());
    None
}

/// `domains[x]` / `hosts[][x]` / `hosts[alt][x]` as wptserve's config answers them for the
/// `web-platform.test` / `not-web-platform.test` pair -- only in a WebSocket URL on the run's servers
/// (see `wpt_substitute`), only when the name resolves, and only for the ASCII subdomains.
fn ws_host(body: &str, req: &WptRequest) -> Option<String> {
    ws_ports()?;
    let (hostname, _) = req.host_port();
    if hostname != "web-platform.test" {
        return None;
    }
    let (base, sub) = if let Some(r) = body.strip_prefix("domains[") {
        ("web-platform.test", r.strip_suffix(']')?)
    } else if let Some(r) = body.strip_prefix("hosts[") {
        let (which, r) = r.split_once("][")?;
        let base = match which {
            "" => "web-platform.test",
            "alt" => "not-web-platform.test",
            _ => return None,
        };
        (base, r.strip_suffix(']')?)
    } else {
        return None;
    };
    let name = match sub {
        "" => base.to_string(),
        "www" | "www1" | "www2" => format!("{sub}.{base}"),
        _ => return None,
    };
    let resolves = std::net::ToSocketAddrs::to_socket_addrs(&(name.as_str(), 80)).map(|mut it| it.next().is_some()).unwrap_or(false);
    resolves.then_some(name)
}

fn substitute_one(body: &str, req: &WptRequest, vars: &mut std::collections::HashMap<String, String>) -> Option<String> {
    enum Tok {
        Var(String),
        Ident(String),
        Index(String),
        Args(Vec<String>),
    }
    // re.Scanner semantics: match tokens from the start; stop at the first character no rule matches.
    let word = |s: &str| s.char_indices().find(|(_, c)| !(c.is_alphanumeric() || *c == '_')).map(|(i, _)| i).unwrap_or(s.len());
    let mut toks = Vec::new();
    let mut s = body;
    loop {
        if let Some(r) = s.strip_prefix('$') {
            let n = word(r);
            if n > 0 && r[n..].starts_with(':') {
                toks.push(Tok::Var(format!("${}", &r[..n])));
                s = &r[n + 1..];
                continue;
            }
            if n > 0 {
                toks.push(Tok::Ident(format!("${}", &r[..n])));
                s = &r[n..];
                continue;
            }
            break;
        }
        let n = word(s);
        if n > 0 {
            toks.push(Tok::Ident(s[..n].to_string()));
            s = &s[n..];
        } else if let Some(r) = s.strip_prefix('[') {
            let end = r.find(']')?;
            toks.push(Tok::Index(r[..end].to_string()));
            s = &r[end + 1..];
        } else if let Some(r) = s.strip_prefix('(') {
            let end = r.find(')')?;
            let inner = &r[..end];
            let args = if inner.is_empty() {
                Vec::new()
            } else {
                // `re.split(r",\s*", ...)`
                inner.split(',').enumerate().map(|(i, a)| if i == 0 { a.to_string() } else { a.trim_start().to_string() }).collect()
            };
            toks.push(Tok::Args(args));
            s = &r[end + 1..];
        } else {
            break;
        }
    }
    let mut toks = toks.into_iter();
    let (assign, field) = match toks.next()? {
        Tok::Var(v) => match toks.next()? {
            Tok::Ident(f) => (Some(v), f),
            _ => return None,
        },
        Tok::Ident(f) => (None, f),
        _ => return None,
    };
    let rest: Vec<Tok> = toks.collect();
    let (hostname, port) = req.host_port();
    let index = |i: usize| -> Option<&str> {
        match rest.get(i) {
            Some(Tok::Index(k)) => Some(k.as_str()),
            _ => None,
        }
    };
    let value: String = if let Some(v) = vars.get(&field) {
        if !rest.is_empty() {
            return None;
        }
        v.clone()
    } else {
        match field.as_str() {
            "host" if rest.is_empty() => hostname,
            "url_base" if rest.is_empty() => "/".to_string(),
            "ports" if rest.len() == 2 && index(0) == Some("http") && index(1) == Some("0") => port,
            // WPT's own WebSocket servers, started for the run (`ws_ports`); verbatim without them.
            "ports" if rest.len() == 2 && index(1) == Some("0") && matches!(index(0), Some("ws" | "wss" | "h2")) => {
                let p = ws_ports()?;
                match index(0)? {
                    "ws" => p.ws,
                    "wss" => p.wss,
                    _ => p.h2,
                }
                .to_string()
            }
            "location" if rest.len() == 1 => match index(0)? {
                "server" => format!("http://{hostname}:{port}"),
                "scheme" => "http".to_string(),
                "host" => format!("{hostname}:{port}"),
                "hostname" => hostname,
                "port" => port,
                "path" | "pathname" => req.path().to_string(),
                "query" => format!("?{}", req.query()),
                _ => return None,
            },
            "GET" if rest.len() == 1 => {
                let name = index(0)?;
                // FirstWrapper: the FIRST value, "" when absent. Blank values are kept by
                // wptserve's own query parser, unlike parse_qs.
                let decode = |s: &str| percent_encoding::percent_decode_str(&s.replace('+', " ")).decode_utf8_lossy().into_owned();
                req.query()
                    .split('&')
                    .filter(|kv| !kv.is_empty())
                    .map(|kv| kv.split_once('=').unwrap_or((kv, "")))
                    .find(|(k, _)| decode(k) == name)
                    .map(|(_, v)| decode(v))
                    .unwrap_or_default()
            }
            "headers" if rest.len() == 1 => req.header(index(0)?)?.to_string(),
            "header_or_default" if rest.len() == 1 => match &rest[0] {
                Tok::Args(a) if a.len() == 2 => req.header(&a[0]).unwrap_or(&a[1]).to_string(),
                _ => return None,
            },
            "uuid" if rest.len() == 1 => match &rest[0] {
                Tok::Args(a) if a.is_empty() => {
                    let mut b = [0u8; 16];
                    getrandom::getrandom(&mut b).ok()?;
                    b[6] = (b[6] & 0x0f) | 0x40;
                    b[8] = (b[8] & 0x3f) | 0x80;
                    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
                    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
                }
                _ => return None,
            },
            "file_hash" if rest.len() == 1 => match &rest[0] {
                Tok::Args(a) if a.len() == 2 => {
                    use base64::Engine as _;
                    use sha2::Digest as _;
                    let bytes = std::fs::read(wpt_root().join(a[1].trim_start_matches('/'))).ok()?;
                    let digest: Vec<u8> = match a[0].as_str() {
                        "sha1" => sha1::Sha1::digest(&bytes).to_vec(),
                        "sha224" => sha2::Sha224::digest(&bytes).to_vec(),
                        "sha256" => sha2::Sha256::digest(&bytes).to_vec(),
                        "sha384" => sha2::Sha384::digest(&bytes).to_vec(),
                        "sha512" => sha2::Sha512::digest(&bytes).to_vec(),
                        _ => return None, // md5: no digest crate in this tree; left verbatim
                    };
                    base64::engine::general_purpose::STANDARD.encode(digest)
                }
                _ => return None,
            },
            _ => return None,
        }
    };
    if let Some(v) = assign {
        vars.insert(v, value.clone());
    }
    Some(value)
}

/// wptserve's `request.GET`: `parse_qsl(query, keep_blank_values=True)`, in order, repeats kept.
fn query_pairs(query: &str) -> Vec<(String, String)> {
    let decode = |s: &str| percent_encoding::percent_decode_str(&s.replace('+', " ")).decode_utf8_lossy().into_owned();
    query
        .split('&')
        .filter(|kv| !kv.is_empty())
        .map(|kv| kv.split_once('=').unwrap_or((kv, "")))
        .map(|(k, v)| (decode(k), decode(v)))
        .collect()
}

/// Python's `urllib.parse.quote_plus`: unreserved `A-Za-z0-9_.-~` kept, space as `+`, rest `%XX`.
fn quote_plus(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// THE REDIRECT HANDLERS, because substituting `/common/get-host-info.sub.js` made them reachable.
/// Before, `get_host_info().HTTP_ORIGIN` was the literal `http://{{host}}:{{ports...`,
/// every fetch to it rejected at URL parsing, and `fetch/api/redirect/redirect-to-dataurl.any.js` --
/// which asserts that a fetch REJECTS -- was on the floor as a pass for that reason alone. With a real
/// origin the fetch reached a 404 and resolved. A floor line that passes because the test could not
/// reach its server is not a measurement, so the handler it needs is emulated, line for line:
///
/// ```text
/// common/redirect.py                 status (302), location, enable-cors
/// xhr/resources/redirect.py          code (302), location (re-decoded), delay, followed; its
///                                    "WEBSRT MARKETING" reason and "Content:Type" name kept as written
/// fetch/api/resources/redirect.py    the stash-free path: Origin echo, OPTIONS preflight,
///                                    redirect_status, location + query + count=1, simple, delay,
///                                    redirect_referrerpolicy
/// ```
///
/// `fetch/api/resources/redirect.py?token=...` needs wptserve's cross-request stash, which this
/// server does not have: it returns `None` and 404s as before, rather than answering a count it
/// cannot know. POST-body `redirect_status` is not read for the same reason (the body is not parsed).
/// An absent required parameter, where wptserve raises, is a 500.
fn redirect_handler(rel: &str, req: &WptRequest) -> Option<(WptResponse, Option<Duration>)> {
    let get = query_pairs(req.query());
    let first = |k: &str| get.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let has = |k: &str| get.iter().any(|(n, _)| n == k);
    let bare = |status: u16| WptResponse { status, headers: Vec::new(), body: Vec::new(), trickle: None, reason: None };
    let server_error = || (WptResponse::plain(500, b"handler raised".to_vec()), None);
    let delay = |ms: Option<String>| -> Result<Option<Duration>, ()> {
        match ms {
            None => Ok(None),
            Some(v) => v.trim().parse::<f64>().map(|f| Some(Duration::from_secs_f64((f / 1000.0).max(0.0)))).map_err(|_| ()),
        }
    };
    match rel {
        "common/redirect.py" => {
            let mut r = bare(first("status").and_then(|v| v.trim().parse::<u16>().ok()).unwrap_or(302));
            let Some(location) = first("location") else { return Some(server_error()) };
            r.set("Location", &location);
            if has("enable-cors") {
                if let Some(origin) = req.header("origin").filter(|o| !o.is_empty()) {
                    r.set("Content-Type", "text/plain");
                    r.set("Access-Control-Allow-Origin", origin);
                    r.set("Access-Control-Allow-Credentials", "true");
                }
            }
            Some((r, None))
        }
        "xhr/resources/redirect.py" => {
            let Ok(code) = first("code").map(|c| c.trim().parse::<u16>()).unwrap_or(Ok(302)) else {
                return Some(server_error());
            };
            let mut location = first("location").unwrap_or_else(|| format!("{}?followed", req.path()));
            if !location.is_empty() {
                // `parse_qs("location=" + location)["location"][0]` -- a SECOND decode.
                match query_last(&format!("location={location}"), "location") {
                    Some(l) => location = l,
                    None => return Some(server_error()),
                }
                if location.starts_with("redirect.py") {
                    location.push_str(&format!("&code={code}"));
                }
            }
            let Ok(d) = delay(first("delay")) else { return Some(server_error()) };
            if has("followed") {
                let mut r = bare(200);
                r.headers.push(("Content:Type".into(), "text/plain".into()));
                r.body = b"MAGIC HAPPENED".to_vec();
                return Some((r, d));
            }
            let mut r = bare(code);
            r.reason = Some("WEBSRT MARKETING".into());
            r.headers.push(("Location".into(), location));
            r.body = b"TEST".to_vec();
            Some((r, d))
        }
        "fetch/api/resources/redirect.py" => {
            if has("token") {
                return None;
            }
            let mut r = bare(302);
            r.headers.push(("Content-Type".into(), "text/plain".into()));
            r.headers.push(("Cache-Control".into(), "no-cache".into()));
            r.headers.push(("Pragma".into(), "no-cache".into()));
            match req.header("origin") {
                Some(origin) => {
                    r.headers.push(("Access-Control-Allow-Origin".into(), origin.to_string()));
                    r.headers.push(("Access-Control-Allow-Credentials".into(), "true".into()));
                }
                None => r.headers.push(("Access-Control-Allow-Origin".into(), "*".into())),
            }
            if req.method == "OPTIONS" {
                if let Some(h) = first("allow_headers") {
                    r.headers.push(("Access-Control-Allow-Headers".into(), h));
                }
                if !has("redirect_preflight") {
                    r.status = 200;
                    return Some((r, None));
                }
            }
            if let Some(v) = first("redirect_status") {
                let Ok(code) = v.trim().parse::<u16>() else { return Some(server_error()) };
                r.status = code;
            }
            if let Some(mut url) = first("location") {
                if !has("simple") {
                    // `urlparse(url).scheme`: a leading `[A-Za-z][A-Za-z0-9+.-]*:`, lower-cased.
                    let scheme = url
                        .split_once(':')
                        .map(|(s, _)| s)
                        .filter(|s| {
                            s.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
                                && s.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
                        })
                        .map(|s| s.to_ascii_lowercase())
                        .unwrap_or_default();
                    if scheme.is_empty() || scheme == "http" || scheme == "https" {
                        url.push(if url.contains('?') { '&' } else { '?' });
                        // A dict built from `request.GET.items()`: first position, first value.
                        let mut seen: Vec<&str> = Vec::new();
                        let mut parts = Vec::new();
                        for (k, _) in &get {
                            if !seen.contains(&k.as_str()) {
                                seen.push(k);
                                parts.push(format!("{}={}", quote_plus(k), quote_plus(&first(k).unwrap_or_default())));
                            }
                        }
                        url.push_str(&parts.join("&"));
                        url.push_str("&count=1");
                    }
                }
                r.headers.push(("Location".into(), url));
            }
            if let Some(p) = first("redirect_referrerpolicy") {
                r.headers.push(("Referrer-Policy".into(), p));
            }
            let Ok(d) = delay(first("delay")) else { return Some(server_error()) };
            Some((r, d))
        }
        _ => None,
    }
}

/// One timed part of a response body: bytes to write, or a pause (`trickle`'s `d<secs>`).
enum BodyPart {
    Bytes(Vec<u8>),
    Delay(Duration),
}

struct WptResponse {
    status: u16,
    /// In wire order; names compared case-insensitively by `set`/`has`, spelled as given.
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    /// `Some` once `trickle` has planned the body; then `body` is ignored and no Content-Length is sent.
    trickle: Option<Vec<BodyPart>>,
    /// A handler's own reason phrase (`(code, message)` in a wptserve handler's return value).
    reason: Option<String>,
}

impl WptResponse {
    fn plain(status: u16, body: Vec<u8>) -> Self {
        let mut r = Self::file("text/plain; charset=utf-8", body);
        r.status = status;
        r
    }

    fn file(ctype: &str, body: Vec<u8>) -> Self {
        WptResponse {
            status: 200,
            headers: vec![("Content-Type".into(), ctype.into()), ("Access-Control-Allow-Origin".into(), "*".into())],
            body,
            trickle: None,
            reason: None,
        }
    }

    fn has(&self, name: &str) -> bool {
        self.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name))
    }

    /// wptserve `ResponseHeaders.set`: drop every existing field of that name, then add one.
    fn set(&mut self, name: &str, value: &str) {
        self.headers.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
        self.headers.push((name.to_string(), value.to_string()));
    }

    fn head(&self, content_length: Option<usize>) -> String {
        let mut h = format!("HTTP/1.1 {} {}\r\n", self.status, self.reason.as_deref().unwrap_or(reason_phrase(self.status)));
        for (k, v) in &self.headers {
            if k.eq_ignore_ascii_case("connection") || k.eq_ignore_ascii_case("content-length") {
                continue;
            }
            h.push_str(&format!("{k}: {v}\r\n"));
        }
        if let Some(n) = content_length {
            h.push_str(&format!("Content-Length: {n}\r\n"));
        }
        h.push_str("Connection: close\r\n\r\n");
        h
    }

    async fn write_to(self, s: &mut tokio::net::TcpStream) {
        match self.trickle {
            None => {
                let _ = s.write_all(self.head(Some(self.body.len())).as_bytes()).await;
                let _ = s.write_all(&self.body).await;
            }
            Some(ref parts) => {
                // Headers first, flushed, exactly as wptserve's writer does before it iterates the
                // content generator -- the delays are in the BODY, not before the status line.
                if s.write_all(self.head(None).as_bytes()).await.is_err() || s.flush().await.is_err() {
                    return;
                }
                for part in parts {
                    match part {
                        BodyPart::Delay(d) => tokio::time::sleep(*d).await,
                        BodyPart::Bytes(b) if b.is_empty() => {}
                        BodyPart::Bytes(b) => {
                            if s.write_all(b).await.is_err() || s.flush().await.is_err() {
                                return;
                            }
                        }
                    }
                }
            }
        }
        let _ = s.flush().await;
    }
}

fn reason_phrase(code: u16) -> &'static str {
    match code {
        100 => "Continue",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        203 => "Non-Authoritative Information",
        204 => "No Content",
        205 => "Reset Content",
        206 => "Partial Content",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        408 => "Request Timeout",
        409 => "Conflict",
        410 => "Gone",
        412 => "Precondition Failed",
        413 => "Request Entity Too Large",
        415 => "Unsupported Media Type",
        416 => "Requested Range Not Satisfiable",
        418 => "I'm a teapot",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Unknown",
    }
}

/// `urllib.parse.parse_qs(query)[name][-1]`: `&`-separated, `+` is a space, percent-decoded, a pair
/// with no `=` dropped, the LAST occurrence wins.
fn query_last(query: &str, name: &str) -> Option<String> {
    let decode = |s: &str| percent_encoding::percent_decode_str(&s.replace('+', " ")).decode_utf8_lossy().into_owned();
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .filter(|(k, v)| decode(k) == name && !v.is_empty())
        .map(|(_, v)| decode(v))
        .last()
}

/// wptserve's `PipeTokenizer`: `name(arg,arg)|name|name(arg)`, `\n` `\r` `\t` escapes in arguments,
/// any other escaped character taken literally.
fn parse_pipeline(spec: &str) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    let mut chars = spec.chars();
    let mut name = String::new();
    loop {
        match chars.next() {
            None => {
                if !name.is_empty() {
                    out.push((std::mem::take(&mut name), Vec::new()));
                }
                return out;
            }
            Some('|') => {
                if !name.is_empty() {
                    out.push((std::mem::take(&mut name), Vec::new()));
                }
            }
            Some('(') => {
                out.push((std::mem::take(&mut name), Vec::new()));
                let mut arg = String::new();
                loop {
                    match chars.next() {
                        None => {
                            out.last_mut().unwrap().1.push(arg);
                            return out;
                        }
                        Some('\\') => match chars.next() {
                            Some('n') => arg.push('\n'),
                            Some('r') => arg.push('\r'),
                            Some('t') => arg.push('\t'),
                            Some(c) => arg.push(c),
                            None => {}
                        },
                        Some(',') => out.last_mut().unwrap().1.push(std::mem::take(&mut arg)),
                        Some(')') => {
                            out.last_mut().unwrap().1.push(arg);
                            break;
                        }
                        Some(c) => arg.push(c),
                    }
                }
            }
            Some(c) => name.push(c),
        }
    }
}

/// `trickle(<spec>)`: `d<secs>` pauses, `<n>` sends n bytes, a final `r<n>` repeats the previous n
/// instructions until the body is sent; otherwise the remainder follows the last instruction. Adjacent
/// instructions of one kind merge, as in pipes.py. `None` is a malformed spec (wptserve raises).
fn trickle_plan(spec: &str, content: &[u8]) -> Option<Vec<BodyPart>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Item {
        Delay(f64),
        Bytes(usize),
        Repeat(usize),
    }
    let mut items: Vec<Item> = Vec::new();
    for tok in spec.split(':') {
        let item = if let Some(v) = tok.strip_prefix('d') {
            Item::Delay(v.trim().parse::<f64>().ok().filter(|f| f.is_finite() && *f >= 0.0)?)
        } else if let Some(v) = tok.strip_prefix('r') {
            let n = v.trim().parse::<usize>().ok()?;
            if n % 2 != 0 {
                return None;
            }
            Item::Repeat(n)
        } else {
            Item::Bytes(tok.trim().parse::<usize>().ok()?)
        };
        match (items.last_mut(), item) {
            (Some(Item::Delay(a)), Item::Delay(b)) => *a += b,
            (Some(Item::Bytes(a)), Item::Bytes(b)) => *a += b,
            (Some(Item::Repeat(a)), Item::Repeat(b)) => *a += b,
            _ => items.push(item),
        }
    }
    let mut parts = Vec::new();
    let mut offset = 0usize;
    fn run(items: &[Item], content: &[u8], offset: &mut usize, parts: &mut Vec<BodyPart>) {
        for (i, it) in items.iter().enumerate() {
            match *it {
                Item::Bytes(n) => {
                    let end = (*offset + n).min(content.len());
                    parts.push(BodyPart::Bytes(content[(*offset).min(end)..end].to_vec()));
                    *offset += n;
                }
                Item::Delay(s) => parts.push(BodyPart::Delay(Duration::from_secs_f64(s))),
                Item::Repeat(n) => {
                    if i != items.len() - 1 {
                        continue;
                    }
                    // `delays[-(value + 1):-1]`: the n instructions before this one.
                    let start = i.saturating_sub(n);
                    let body = &items[start..i];
                    // A repeat of nothing that sends bytes would spin forever in wptserve too; refuse
                    // it here instead of hanging the shard.
                    if !body.iter().any(|b| matches!(b, Item::Bytes(k) if *k > 0)) {
                        return;
                    }
                    while *offset < content.len() {
                        run(body, content, offset, parts);
                    }
                }
            }
        }
    }
    run(&items, content, &mut offset, &mut parts);
    if offset < content.len() {
        parts.push(BodyPart::Bytes(content[offset..].to_vec()));
    }
    Some(parts)
}

/// wptserve `load_headers`: `__dir__{.sub}.headers` then `<file>{.sub}.headers`, one `Name: value` per
/// non-empty line, concatenated in that order. A `.sub.headers` file is substituted first.
fn load_headers(root: &Path, rel: &str, req: &WptRequest) -> Vec<(String, String)> {
    let one = |base: String| -> Vec<(String, String)> {
        let sub_path = root.join(format!("{base}.sub.headers"));
        let (data, sub) = match std::fs::read(&sub_path) {
            Ok(d) => (d, true),
            Err(_) => match std::fs::read(root.join(format!("{base}.headers"))) {
                Ok(d) => (d, false),
                Err(_) => return Vec::new(),
            },
        };
        let data = if sub { wpt_substitute(&data, req, false) } else { data };
        String::from_utf8_lossy(&data)
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| match l.split_once(':') {
                Some((k, v)) => (k.trim().to_string(), v.trim().to_string()),
                None => (l.trim().to_string(), String::new()),
            })
            .collect()
    };
    let dir = rel.rsplit_once('/').map(|(d, _)| format!("{d}/__dir__")).unwrap_or_else(|| "__dir__".to_string());
    let mut out = one(dir);
    out.extend(one(rel.to_string()));
    out
}

/// A static corpus file served the way wptserve's FileHandler serves it: sidecar headers, then the
/// `.sub.` / `?pipe=` pipeline. A pipeline wptserve would reject answers 500, as wptserve does.
fn static_file_response(root: &Path, rel: &str, req: &WptRequest, bytes: Vec<u8>) -> WptResponse {
    let mut resp = WptResponse::file(content_type(rel), bytes);
    let sidecar = load_headers(root, rel, req);
    // A sidecar field replaces this server's default of the same name (the guessed Content-Type, and
    // the blanket `Access-Control-Allow-Origin: *`) rather than doubling it into a list.
    resp.headers.retain(|(k, _)| !sidecar.iter().any(|(n, _)| n.eq_ignore_ascii_case(k)));
    resp.headers.extend(sidecar);

    let query = req.query();
    let mut pipes: Vec<(String, Vec<String>)> = Vec::new();
    if rel.contains(".sub.") {
        let ext = rel.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
        let ml = matches!(ext.as_str(), "html" | "htm" | "xht" | "xhtml" | "xml" | "svg");
        pipes.push(("sub".into(), vec![if ml { "html".into() } else { "none".into() }]));
    }
    if let Some(p) = query_last(query, "pipe") {
        pipes.extend(parse_pipeline(&p));
    }
    for (name, args) in pipes {
        if apply_pipe(&mut resp, &name, &args, req).is_none() {
            return WptResponse::plain(500, format!("wptserve pipe {name}({}) failed", args.join(",")).into_bytes());
        }
    }
    resp
}

/// One wptserve pipe. `None` is what wptserve turns into a 500: an unknown name, a wrong argument
/// count, or an argument its converter rejects.
fn apply_pipe(resp: &mut WptResponse, name: &str, args: &[String], req: &WptRequest) -> Option<()> {
    // Every pipe after `trickle` sees the content trickle already resolved; wptserve's pipes that read
    // content (`slice`, `sub`) call `resolve_content`, which walks the generator -- sleeping. Keep
    // the plan and act on the bytes it would yield, delays preserved.
    let content = |resp: &WptResponse| -> Vec<u8> {
        match &resp.trickle {
            None => resp.body.clone(),
            Some(parts) => parts
                .iter()
                .flat_map(|p| match p {
                    BodyPart::Bytes(b) => b.clone(),
                    BodyPart::Delay(_) => Vec::new(),
                })
                .collect(),
        }
    };
    let nullable_int = |s: &str| -> Option<Option<i64>> {
        if s.eq_ignore_ascii_case("null") { Some(None) } else { s.trim().parse::<i64>().ok().map(Some) }
    };
    match name {
        "status" => {
            if args.len() != 1 {
                return None;
            }
            resp.status = args[0].trim().parse::<u16>().ok()?;
        }
        "header" => {
            if !(2..=3).contains(&args.len()) {
                return None;
            }
            let append = match args.get(2).map(|a| a.to_ascii_lowercase()) {
                None => false,
                Some(a) if a == "true" || a == "1" => true,
                Some(a) if a == "false" || a == "0" => false,
                Some(_) => return None,
            };
            if append {
                resp.headers.push((args[0].clone(), args[1].clone()));
            } else {
                resp.set(&args[0], &args[1]);
            }
        }
        "trickle" => {
            if args.len() != 1 {
                return None;
            }
            let body = content(resp);
            if args[0].is_empty() {
                return Some(());
            }
            let plan = trickle_plan(&args[0], &body)?;
            if !(resp.has("Cache-Control") || resp.has("Pragma") || resp.has("Expires")) {
                resp.set("Cache-Control", "no-cache, no-store, must-revalidate");
                resp.set("Pragma", "no-cache");
                resp.set("Expires", "0");
            }
            resp.trickle = Some(plan);
        }
        "slice" => {
            if !(1..=2).contains(&args.len()) {
                return None;
            }
            let body = content(resp);
            let len = body.len() as i64;
            // Python slice bounds: negative counts from the end, both clamp to [0, len].
            let clamp = |v: i64| if v < 0 { (len + v).max(0) } else { v.min(len) };
            let start = nullable_int(&args[0])?.map(clamp).unwrap_or(0);
            let end = match args.get(1) {
                None => len,
                Some(a) => nullable_int(a)?.map(clamp).unwrap_or(len),
            };
            resp.body = if start < end { body[start as usize..end as usize].to_vec() } else { Vec::new() };
            resp.trickle = None;
        }
        "sub" => {
            if args.len() > 1 {
                return None;
            }
            let html = match args.first().map(|a| a.to_ascii_lowercase()) {
                None => true,
                Some(a) if a == "html" => true,
                Some(a) if a == "none" || a == "null" => false,
                Some(_) => return None,
            };
            resp.body = wpt_substitute(&content(resp), req, html);
            resp.trickle = None;
        }
        _ => return None,
    }
    Some(())
}

/// The emulated wptserve features, pinned against pipes.py / handlers.py behaviour. Non-ignored and
/// cheap: pure functions plus one in-process server on 127.0.0.1, no engine.
#[cfg(test)]
mod wptserve_emulation {
    use super::*;

    fn plan_shape(spec: &str, content: &[u8]) -> Vec<String> {
        trickle_plan(spec, content)
            .expect("well-formed spec")
            .iter()
            .map(|p| match p {
                BodyPart::Delay(d) => format!("d{}", d.as_secs_f64()),
                BodyPart::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
            })
            .collect()
    }

    #[test]
    fn pipe_strings_tokenise_like_pipe_tokenizer() {
        assert_eq!(parse_pipeline("trickle(d1)"), vec![("trickle".to_string(), vec!["d1".to_string()])]);
        assert_eq!(
            parse_pipeline("status(404)|header(X-A,b\\,c)|sub"),
            vec![
                ("status".to_string(), vec!["404".to_string()]),
                ("header".to_string(), vec!["X-A".to_string(), "b,c".to_string()]),
                ("sub".to_string(), vec![]),
            ]
        );
        assert_eq!(parse_pipeline("header(A,x\\ny,True)")[0].1, vec!["A", "x\ny", "True"]);
    }

    #[test]
    fn query_takes_the_last_decoded_pipe() {
        assert_eq!(query_last("pipe=trickle(d1)&0.5", "pipe").as_deref(), Some("trickle(d1)"));
        assert_eq!(query_last("pipe=a&pipe=header(X,a%20b)", "pipe").as_deref(), Some("header(X,a b)"));
        assert_eq!(query_last("x=1", "pipe"), None);
    }

    #[test]
    fn trickle_delays_come_before_the_bytes_they_hold_back() {
        // `d1`: pause, then the whole remainder -- the shape all 37 execution-timing files use.
        assert_eq!(plan_shape("d1", b"abcdef"), vec!["d1", "abcdef"]);
        // Adjacent instructions of one kind merge.
        assert_eq!(plan_shape("d1:d0.5:2:1", b"abcdef"), vec!["d1.5", "abc", "def"]);
        // `rN` repeats the previous N instructions until the body is out, with no trailing remainder.
        assert_eq!(plan_shape("2:d0.25:r2", b"abcde"), vec!["ab", "d0.25", "cd", "d0.25", "e", "d0.25"]);
        assert!(trickle_plan("r3", b"x").is_none(), "an odd repeat count is rejected, as in pipes.py");
        assert!(trickle_plan("dx", b"x").is_none());
    }

    #[test]
    fn substitution_answers_what_one_origin_can_and_leaves_other_origins_verbatim() {
        let req = WptRequest::parse("GET /a/b.sub.js?x=1&y=%3Cz%3E HTTP/1.1\r\nHost: 127.0.0.1:8123\r\nX-T: hi\r\n\r\n");
        let got = |s: &str, html: bool| String::from_utf8(wpt_substitute(s.as_bytes(), &req, html)).unwrap();
        assert_eq!(got("{{host}}:{{ports[http][0]}}", false), "127.0.0.1:8123");
        assert_eq!(got("{{location[server]}}{{location[path]}}{{location[query]}}", false), "http://127.0.0.1:8123/a/b.sub.js?x=1&y=%3Cz%3E");
        assert_eq!(got("{{GET[y]}}|{{GET[x]}}|{{GET[nope]}}", false), "<z>|1|");
        assert_eq!(got("{{GET[y]}}", true), "&lt;z&gt;");
        assert_eq!(got("{{headers[x-t]}} {{header_or_default(X-None, dflt)}}", false), "hi dflt");
        assert_eq!(got("{{$id:uuid()}}={{$id}}", false).split('=').collect::<Vec<_>>().windows(2).all(|w| w[0] == w[1]), true);
        assert_eq!(got("{{domains[www]}} {{ports[https][0]}}", false), "{{domains[www]}} {{ports[https][0]}}");
        assert_eq!(got("no placeholders {", false), "no placeholders {");
    }

    #[test]
    fn the_redirect_handlers_answer_as_their_python_does() {
        let req = |line: &str, extra: &str| WptRequest::parse(&format!("{line} HTTP/1.1\r\nHost: 127.0.0.1:9\r\n{extra}\r\n"));
        let hdr = |r: &WptResponse, n: &str| r.headers.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone());

        let (r, d) = redirect_handler("common/redirect.py", &req("GET /common/redirect.py?status=307&location=/x%3Fy", "")).unwrap();
        assert_eq!((r.status, hdr(&r, "Location").as_deref(), d), (307, Some("/x?y"), None));
        let (r, _) = redirect_handler("common/redirect.py", &req("GET /common/redirect.py", "")).unwrap();
        assert_eq!(r.status, 500, "a missing location raises in the handler");

        let (r, _) = redirect_handler("xhr/resources/redirect.py", &req("GET /xhr/resources/redirect.py?code=301", "")).unwrap();
        assert_eq!((r.status, r.reason.as_deref()), (301, Some("WEBSRT MARKETING")));
        assert_eq!(hdr(&r, "Location").as_deref(), Some("/xhr/resources/redirect.py?followed"));
        let (r, _) = redirect_handler("xhr/resources/redirect.py", &req("GET /xhr/resources/redirect.py?followed", "")).unwrap();
        assert_eq!((r.status, r.body.as_slice()), (200, &b"MAGIC HAPPENED"[..]));

        let url = "GET /fetch/api/resources/redirect.py?cors&location=data%3Atext%2Fplain%2Cx";
        let (r, _) = redirect_handler("fetch/api/resources/redirect.py", &req(url, "")).unwrap();
        assert_eq!(r.status, 302);
        assert_eq!(hdr(&r, "Location").as_deref(), Some("data:text/plain,x"), "a non-http scheme gets no query appended");
        assert_eq!(hdr(&r, "Access-Control-Allow-Origin").as_deref(), Some("*"));
        let url = "GET /fetch/api/resources/redirect.py?redirect_status=303&location=%2Fa%3Fb%3D1&x=a%20b";
        let (r, _) = redirect_handler("fetch/api/resources/redirect.py", &req(url, "Origin: http://o\r\n")).unwrap();
        assert_eq!(r.status, 303);
        assert_eq!(
            hdr(&r, "Location").as_deref(),
            Some("/a?b=1&redirect_status=303&location=%2Fa%3Fb%3D1&x=a+b&count=1")
        );
        assert_eq!(hdr(&r, "Access-Control-Allow-Origin").as_deref(), Some("http://o"));
        let (r, _) = redirect_handler("fetch/api/resources/redirect.py", &req("OPTIONS /fetch/api/resources/redirect.py?location=/a", "")).unwrap();
        assert_eq!((r.status, hdr(&r, "Location")), (200, None), "a preflight is not redirected");
        assert!(redirect_handler("fetch/api/resources/redirect.py", &req("GET /fetch/api/resources/redirect.py?token=t", "")).is_none());
    }

    /// The server end to end: sidecar headers (`__dir__` first), `status`/`header` pipes, a 500 for a
    /// pipe wptserve does not have, and `trickle` holding the BODY back while the headers go out.
    #[tokio::test]
    async fn the_static_server_applies_headers_files_and_pipes() {
        let dir = std::env::temp_dir().join(format!("korvus-wpt-pipes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("d")).unwrap();
        std::fs::write(dir.join("d/s.js"), "var x = 1;\n").unwrap();
        std::fs::write(dir.join("d/__dir__.headers"), "X-Dir: 1\n").unwrap();
        std::fs::write(dir.join("d/s.js.headers"), "Content-Type: text/x-custom\nX-File: 2\n").unwrap();
        std::fs::write(dir.join("d/v.sub.txt"), "{{GET[q]}}@{{location[port]}}").unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve(listener, dir.clone()));

        // Returns (head, time the head arrived, body, time the body finished).
        let get = |target: &'static str| async move {
            let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
            s.write_all(format!("GET {target} HTTP/1.1\r\nHost: {addr}\r\n\r\n").as_bytes()).await.unwrap();
            let t0 = std::time::Instant::now();
            let mut buf = Vec::new();
            let mut head_at = None;
            let mut chunk = [0u8; 4096];
            loop {
                let n = s.read(&mut chunk).await.unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                if head_at.is_none() && buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    head_at = Some(t0.elapsed());
                }
            }
            let sep = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
            (String::from_utf8_lossy(&buf[..sep]).into_owned(), head_at.unwrap(), buf[sep..].to_vec(), t0.elapsed())
        };

        let (head, _, body, _) = get("/d/s.js").await;
        assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{head}");
        assert!(head.contains("Content-Type: text/x-custom\r\n") && !head.contains("text/javascript"), "{head}");
        let dir_at = head.find("X-Dir: 1").expect("__dir__.headers");
        assert!(dir_at < head.find("X-File: 2").expect("s.js.headers"), "__dir__ first: {head}");
        assert_eq!(body, b"var x = 1;\n");

        let (head, _, _, _) = get("/d/s.js?pipe=status(404)|header(X-File,3)|header(X-File,4,true)").await;
        assert!(head.starts_with("HTTP/1.1 404 Not Found\r\n"), "{head}");
        assert!(!head.contains("X-File: 2") && head.contains("X-File: 3\r\nX-File: 4\r\n"), "{head}");

        let (head, _, _, _) = get("/d/s.js?pipe=nosuchpipe").await;
        assert!(head.starts_with("HTTP/1.1 500 "), "{head}");

        let (_, _, body, _) = get("/d/v.sub.txt?q=hello").await;
        assert_eq!(String::from_utf8(body).unwrap(), format!("hello@{}", addr.port()));

        let (head, head_at, body, done_at) = get("/d/s.js?pipe=trickle(d0.6)").await;
        assert_eq!(body, b"var x = 1;\n");
        assert!(!head.contains("Content-Length"), "a trickled body is close-delimited: {head}");
        assert!(head.contains("Cache-Control: no-cache, no-store, must-revalidate"), "{head}");
        assert!(head_at < Duration::from_millis(400), "headers are not delayed: {head_at:?}");
        assert!(done_at >= Duration::from_millis(600), "the body is: {done_at:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// A real `.html`/`.xhtml` WPT test: served-and-loaded verbatim (its own `<script src>` chain).
    Html,
    /// A real `.any.js`/`.window.js` WPT test: wptserve auto-generates a `<name>.any.html` wrapper that
    /// loads testharness.js + testharnessreport.js + any `// META: script=` helpers + the test body as
    /// classic scripts. We reproduce exactly that wrapper.
    AnyJs,
}

struct TestFile {
    /// Canonical upstream path (web-platform-tests/wpt@master), relative to fixtures/wpt, forward slashes.
    rel: String,
    kind: Kind,
    /// Scoreboard area bucket (e.g. "dom/nodes", "encoding", "url", "custom-elements").
    area: String,
    /// The `<meta name="variant">` / `// META: variant=` query string for this run, `?`-prefixed
    /// (e.g. `"?1-1000"`), or empty for a file that declares none. WPT treats each variant as its
    /// OWN test URL; `/common/subset-tests.js` reads `location.search` and runs only that slice.
    /// Ignoring it does not merely mislabel the run -- it collapses every slice into one page load,
    /// which is how `euckr-encode-href.html` came to declare 17,048 subtests in a single file and
    /// then hang. See `variants_of`.
    variant: String,
    /// The served path when it differs from `rel` -- only the manifest sets this, because a `.any.js`
    /// is addressed as a GENERATED wrapper (`foo.any.html`, `foo.any.worker.html`) that is not a file
    /// on disk. `None` means "addressed as its own path", which is every directory-walk discovery.
    url_path: Option<String>,
}

impl TestFile {
    /// The path as WPT itself addresses this test: `foo.html?1-1000`. Two variants of one file are
    /// two tests, and a scoreboard that prints them under one name silently overwrites or double-counts.
    fn id(&self) -> String {
        match &self.url_path {
            // Under the manifest, two scopes of one `.any.js` are two tests with the same `rel`;
            // keying on the served path is what keeps them apart.
            Some(u) => format!("{}{}", u, self.variant),
            None => format!("{}{}", self.rel, self.variant),
        }
    }
}

/// The variants a test file declares, `?`-prefixed, or a single empty string when it declares none.
///
/// HTML files use `<meta name="variant" content="?1-1000">`; `.any.js`/`.window.js` files use
/// `// META: variant=?1-1000`. Both may appear many times, and the content may be empty (`content=""`
/// means "also run the bare URL"), which is why the empty string is a legitimate variant rather than
/// a signal of absence.
fn variants_of(content: &str, kind: Kind) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    match kind {
        Kind::Html => {
            // Cheap, order-preserving scan: `name="variant"` then the `content="..."` beside it.
            // A real parser is not warranted -- these appear only in a document head, one per line.
            for line in content.lines() {
                let l = line.trim();
                if !l.starts_with("<meta") || !l.contains("name=\"variant\"") {
                    continue;
                }
                if let Some(rest) = l.split_once("content=\"").map(|(_, r)| r) {
                    if let Some((v, _)) = rest.split_once('"') {
                        out.push(v.to_string());
                    }
                }
            }
        }
        Kind::AnyJs => {
            for line in content.lines() {
                let l = line.trim();
                if let Some(v) = l.strip_prefix("// META:").map(str::trim).and_then(|m| m.strip_prefix("variant=")) {
                    out.push(v.trim().to_string());
                }
            }
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Derive the scoreboard area from a path: two segments under dom/ and html/ (dom/nodes, dom/events,
/// html/dom, html/semantics), one segment elsewhere (encoding, url, custom-elements).
fn area_of(rel: &str) -> String {
    let segs: Vec<&str> = rel.split('/').collect();
    if segs.len() >= 3 && (segs[0] == "dom" || segs[0] == "html") {
        format!("{}/{}", segs[0], segs[1])
    } else {
        segs.first().copied().unwrap_or("misc").to_string()
    }
}

/// The interface a WPT file exercises, by the `Interface-member.html` / `Interface.any.js` naming
/// convention: the basename segment before the first '-' (stacked extensions stripped). Mirrors
/// the gen crate's `wpt.rs` `basename_iface`, kept self-contained here (korvus-runtime does not
/// depend on the gen tool). Drives the `KORVUS_WPT_FILTER` per-interface filter below.
fn basename_iface(rel: &str) -> String {
    let mut base = rel.rsplit('/').next().unwrap_or(rel);
    const EXTS: [&str; 8] = ["html", "xhtml", "any", "window", "sub", "tentative", "js", "json"];
    loop {
        let mut changed = false;
        for ext in EXTS {
            if let Some(s) = base.strip_suffix(ext).and_then(|s| s.strip_suffix('.')) {
                base = s;
                changed = true;
                break;
            }
        }
        if !changed {
            break;
        }
    }
    base.split('-').next().unwrap_or(base).split('.').next().unwrap_or(base).to_string()
}

/// Recursively discover test files under `dir`. `resources/` and `common/` hold harness + shared
/// helpers (served, never run). `.any.js`/`.window.js` are JS tests (worker-only globals skipped);
/// `.html`/`.xhtml` that reference testharness.js are page tests. Plain `.js`/`.json` are helpers.
/// Path prefixes to skip entirely (`KORVUS_WPT_EXCLUDE`, comma-separated, e.g. `html/canvas,html/rendering`).
///
/// The tier manifest excludes whole subtrees -- html/canvas, html/rendering, html/editing,
/// html/interaction -- and scoring drops them afterwards, so a run that walks them does thousands of
/// files of work whose results are thrown away. Worse, it does so UNEVENLY: html/canvas alone is large
/// enough that the two shards holding it ran twice as long as the other nine, which puts the imbalance
/// right back where per-URL sharding had removed it.
fn excluded_prefixes() -> Vec<String> {
    std::env::var("KORVUS_WPT_EXCLUDE")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Enumerate tests from WPT's own `MANIFEST.json` (`KORVUS_WPT_MANIFEST`), the way
/// the reference benchmark's `crates/wpt-runner/src/manifest.rs` does.
///
/// This exists to answer one question honestly: are we running the same tests they are? Our own
/// `discover()` walks directories and applies three exclusions of its own -- files that create a
/// nested browsing context, the custom-elements helper suite, worker-only tests -- which drop 866 of
/// the 5,545 core files. The manifest has no opinions: it is WPT's canonical list, it expands a
/// `.any.js` into its `.any.html` / `.any.worker.html` / `.window.html` scopes, and it carries each
/// file's variants. Core is 8,138 URLs there against 5,789 here.
///
/// Shape: `items.testharness` is a directory tree whose leaves are
/// `[hash, [url_or_null, options], ...]`; each pair after the hash is one runnable URL, and a null
/// url means "use the file path".
/// GET a URL over plain HTTP/1.1 and return the body.
///
/// Deliberately hand-rolled over `TcpStream` rather than pulling an HTTP client into the test's
/// dev-dependencies: it talks to one known-good local server, needs no TLS, no redirects, no
/// compression, and a client crate would be a build-time cost for four lines of protocol.
///
/// This exists because with a real `wpt serve` the page body must come FROM THE SERVER. Reading the
/// file off disk is right for a plain `.html` and wrong for everything wptserve generates: a `.sub.`
/// file's placeholders are substituted at request time, and `foo.any.html` has no file behind it at
/// all -- the server composes it from `foo.any.js`. Handing the engine the on-disk bytes for those is
/// what leaves them unable to reach a verdict.
async fn http_get(url: &str) -> Option<(String, Vec<(String, String)>)> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let rest = url.strip_prefix("http://")?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let host = hostport.split(':').next().unwrap_or(hostport);
    let mut sock = tokio::time::timeout(Duration::from_secs(10), tokio::net::TcpStream::connect(hostport)).await.ok()?.ok()?;
    let req = format!("GET {path} HTTP/1.1\r\nHost: {hostport}\r\nUser-Agent: korvus-wpt\r\nAccept: */*\r\nConnection: close\r\n\r\n");
    let _ = host;
    sock.write_all(req.as_bytes()).await.ok()?;
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(20), sock.read_to_end(&mut buf)).await.ok()?.ok()?;
    // Split headers from body on the first CRLFCRLF. A 4xx/5xx still returns its body, which is the
    // honest thing to hand the engine -- the test then fails on content rather than silently on nothing.
    let sep = buf.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
    // The RESPONSE HEADERS come back too, because some of them decide what the document IS rather
    // than what it says: a `Cross-Origin-Opener-Policy` on the test page changes whether the popups
    // that page opens keep their opener. They used to be split off here and dropped on the floor.
    // Repeats are KEPT as separate entries, in wire order -- a repeated field is a list, and a list
    // is not a valid structured-field item, which `header-parsing-repeated` asserts by name.
    let head = String::from_utf8_lossy(&buf[..sep]);
    let mut headers = Vec::new();
    for line in head.split("\r\n").skip(1) {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    // BOM-aware, because 30 core files are UTF-16 and `from_utf8_lossy` shreds every tag in them.
    Some((decode_document(&buf[sep..]), headers))
}

/// The response headers wptserve WOULD send for a file this runner read off disk.
///
/// `http_get` speaks plain HTTP only, so a `.https.` document's body still comes from the checkout
/// (see `build`), and with it no headers at all -- which for `html/cross-origin-opener-policy` means
/// the one fact those files are ABOUT never arrived. wptserve's static handler builds that response
/// from a sidecar: for `foo.html` it reads `foo.html.headers`, one `Name: value` per line. Reading
/// the same sidecar here is the same choice already made for the body, applied to the rest of the
/// response, and it touches only the ~90 core files that have one.
///
/// It is the SAME `load_headers` the static server uses for every file it serves, so the
/// directory's `__dir__.headers` comes first and a `.sub.headers` sidecar is substituted, as wptserve
/// does. Names are lower-cased for the engine, and a `#` line is still not a header.
fn sidecar_headers(root: &Path, rel: &str, base: &str) -> Vec<(String, String)> {
    load_headers(root, rel, &WptRequest::for_url(base))
        .into_iter()
        .filter(|(k, _)| !k.is_empty() && !k.starts_with('#'))
        .map(|(k, v)| (k.to_ascii_lowercase(), v))
        .collect()
}

/// Read a PRE-EXPANDED test list (`KORVUS_WPT_LIST`): one test per line, `rel<TAB>url_path<TAB>variant`.
///
/// Exists to stop eleven shards each parsing the same 40 MB MANIFEST.json. That was ~440 MB of
/// transient allocation and eleven full walks of a large JSON tree, all producing the same answer, and
/// it was a real cost rather than an untidy one: on this machine it drove memory PRESSURE (not memory
/// exhaustion -- 60 GB was mostly free) past systemd-oomd's 50% default, and oomd kills the whole
/// terminal cgroup, which took the terminal and the session with it five times in a day.
///
/// The driver expands the manifest once and hands each shard only its own lines. Same tests, same
/// order, one parse.
fn discover_from_list(path: &Path, out: &mut Vec<TestFile>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!("KORVUS_WPT_LIST: cannot read {}", path.display());
        return;
    };
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut it = line.split('\t');
        let (Some(rel), Some(url), Some(variant)) = (it.next(), it.next(), it.next()) else { continue };
        let lower = rel.to_ascii_lowercase();
        let kind = if lower.ends_with(".any.js") || lower.ends_with(".window.js") { Kind::AnyJs } else { Kind::Html };
        out.push(TestFile {
            rel: rel.to_string(),
            kind,
            area: area_of(rel),
            variant: variant.to_string(),
            url_path: Some(url.to_string()),
        });
    }
}

fn discover_from_manifest(path: &Path, out: &mut Vec<TestFile>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!("KORVUS_WPT_MANIFEST: cannot read {}", path.display());
        return;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        eprintln!("KORVUS_WPT_MANIFEST: {} is not JSON", path.display());
        return;
    };
    let Some(th) = v.pointer("/items/testharness") else {
        eprintln!("KORVUS_WPT_MANIFEST: no items.testharness");
        return;
    };
    fn walk(node: &serde_json::Value, prefix: &str, out: &mut Vec<TestFile>) {
        match node {
            serde_json::Value::Object(map) => {
                for (k, child) in map {
                    let next = if prefix.is_empty() { k.clone() } else { format!("{prefix}/{k}") };
                    walk(child, &next, out);
                }
            }
            serde_json::Value::Array(variants) => {
                for var in variants.iter().skip(1) {
                    let url = var
                        .as_array()
                        .and_then(|a| a.first())
                        .and_then(|u| u.as_str())
                        .map(|u| u.trim_start_matches('/').to_string())
                        .unwrap_or_else(|| prefix.to_string());
                    // The manifest URL is the SERVED path, which for a `.any.js` is a generated
                    // wrapper (`foo.any.worker.html`) with no file behind it. Keep both: `rel` is the
                    // source we read, `url_path` is what the harness is addressed as.
                    let (path_part, variant) = match url.split_once('?') {
                        Some((p, q)) => (p.to_string(), format!("?{q}")),
                        None => (url.clone(), String::new()),
                    };
                    let lower = prefix.to_ascii_lowercase();
                    let kind = if lower.ends_with(".any.js") || lower.ends_with(".window.js") {
                        Kind::AnyJs
                    } else {
                        Kind::Html
                    };
                    out.push(TestFile {
                        rel: prefix.to_string(),
                        kind,
                        area: area_of(prefix),
                        variant,
                        url_path: Some(path_part),
                    });
                }
            }
            _ => {}
        }
    }
    walk(th, "", out);
}

/// The worker scopes a `.any.js` expands to in the manifest.
///
/// W1.6 (worker-url-loading unit): `new Worker(url)`/`new SharedWorker(url)` now actually fetch and run
/// their script (core/korvus-runtime/src/worker.rs), and `importScripts` works -- so `run_file` no
/// longer skips these; the page (`fetch_tests_from_worker(new Worker(...))`, the real wptserve-generated
/// wrapper) runs for real and reports through the SAME `globalThis.__wptresults` plumbing every other
/// test uses (`fetch_tests_from_worker` is `testharness.js`'s own bridge from a worker's results back to
/// the window harness, so nothing here needed to change to receive them). `is_worker_scope` itself is
/// kept: `.any.serviceworker.html` still has no registration-to-verdict path (ServiceWorker registration
/// is source-only, W1.6 stopped at Worker + SharedWorker per the unit brief), so THOSE still report 0/0
/// rather than hang on a `connect`/`fetch` that never arrives.
fn is_worker_scope(url_path: &str) -> bool {
    url_path.ends_with(".any.worker.html")
        || url_path.ends_with(".any.sharedworker.html")
        || url_path.ends_with(".any.serviceworker.html")
        || url_path.ends_with(".any.worker-module.html")
}

/// Of `is_worker_scope`'s four shapes, which ones W1.6 actually gives a real load-and-run path to. A
/// `.any.worker-module.html` worker is created with `{type: "module"}`; the constructor here does not
/// branch on `options` at all (the fetched top script always runs as classic, non-module, code via
/// indirect `eval`), so a real module worker would run its ESM source as a syntax error -- reported
/// (correctly) as a failure, not silently skipped, but not claimed as "loads" either.
///
/// 4.3b adds `.any.serviceworker.html`: `navigator.serviceWorker.register(url)` now fetches and runs the
/// script on the SW isolate's own thread, the realm is a real ServiceWorkerGlobalScope, and the page's
/// `serviceWorker.postMessage({type:"connect"})` opens the client MessagePort the results come back on --
/// so these have a registration-to-verdict path and are no longer declined. What this function refuses is
/// still exactly what it always refused: running a worker-scope test in the WINDOW. A scope it declines
/// reports 0/0; it never falls back to the wrong global.
fn worker_scope_actually_runs(url_path: &str) -> bool {
    url_path.ends_with(".any.worker.html")
        || url_path.ends_with(".any.sharedworker.html")
        || url_path.ends_with(".any.serviceworker.html")
}

fn discover(root: &Path, dir: &Path, out: &mut Vec<TestFile>) {
    {
        let rel = dir.strip_prefix(root).map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default();
        let skip = excluded_prefixes();
        if !rel.is_empty() && skip.iter().any(|p| rel == *p || rel.starts_with(&format!("{p}/"))) {
            return;
        }
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let p = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if p.is_dir() {
            if name == "resources" || name == "common" {
                continue; // helpers/harness only
            }
            discover(root, &p, out);
            continue;
        }
        let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".any.js") || lower.ends_with(".window.js") {
            // Skip worker-only tests: an explicit `// META: global=` that excludes window can't run in a
            // window harness. (Absent or window-inclusive globals run.)
            let content = std::fs::read_to_string(&p).unwrap_or_default();
            let worker_only = content.lines().find_map(|l| {
                let l = l.trim();
                l.strip_prefix("// META:")
                    .map(str::trim)
                    .and_then(|m| m.strip_prefix("global="))
                    .map(|g| !g.split(',').any(|t| t.trim() == "window"))
            });
            if worker_only == Some(true) {
                continue;
            }
            for v in variants_of(&content, Kind::AnyJs) {
                out.push(TestFile { rel: rel.clone(), kind: Kind::AnyJs, area: area_of(&rel), variant: v, url_path: None });
            }
        } else if lower.ends_with(".html") || lower.ends_with(".xhtml") {
            let content = std::fs::read_to_string(&p).unwrap_or_default();
            if !content.contains("testharness.js") {
                continue; // not a testharness test (e.g. a reference/support file)
            }
            // STATIC subset: skip tests that need a nested browsing context. Korvus has no real
            // subframe navigation, so those can't settle -- and the custom-elements reaction machinery
            // spins a synchronous microtask storm that V8 runs to completion uninterruptibly (one file
            // cost 21s).
            //
            // The test is whether the file actually CREATES a frame, not whether the six letters
            // "iframe" occur in it. A substring match excluded 129 files across the swept areas that
            // merely name it -- in a comment, in a `document.createElement` list, in an assertion
            // message -- and those produced no result at all: not a failure, not a timeout, absent even
            // from the file count. A silent exclusion reads as coverage.
            let makes_a_frame = content.contains("<iframe")
                || content.contains("<IFRAME")
                || content.contains("createElement(\"iframe\")")
                || content.contains("createElement('iframe')")
                || content.contains("<frame ")
                || content.contains("<frameset");
            if makes_a_frame || content.contains("custom-elements-helpers.js") {
                continue;
            }
            for v in variants_of(&content, Kind::Html) {
                out.push(TestFile { rel: rel.clone(), kind: Kind::Html, area: area_of(&rel), variant: v, url_path: None });
            }
        }
    }
}

/// Normalize a POSIX-style path (resolve `.`/`..` segments) for META-script resolution.
fn norm(path: &str) -> String {
    let mut stack: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            s => stack.push(s),
        }
    }
    stack.join("/")
}

/// The absolute server paths of a `.any.js` test's `// META: script=` helpers, in order. Relative refs
/// resolve against the test's directory; absolute refs (leading `/`) are used as-is.
fn meta_scripts(content: &str, rel: &str) -> Vec<String> {
    let dir = rel.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let mut v = Vec::new();
    for line in content.lines() {
        let l = line.trim();
        let Some(m) = l.strip_prefix("// META:").map(str::trim) else {
            // META block is at the top; the first non-comment, non-blank line ends it.
            if !l.is_empty() && !l.starts_with("//") {
                break;
            }
            continue;
        };
        if let Some(s) = m.strip_prefix("script=") {
            let s = s.trim();
            if s.contains("testharness") {
                continue; // testharness.js + report hook are added explicitly
            }
            let abs = if let Some(rest) = s.strip_prefix('/') {
                norm(rest)
            } else {
                norm(&format!("{dir}/{s}"))
            };
            v.push(format!("/{abs}"));
        }
    }
    v
}

/// Build the top-level document + its server URL for a test. For `.html`, the file verbatim (loaded at
/// its own URL so relative `<script src>` resolve). For `.any.js`, the wptserve-equivalent `.any.html`
/// wrapper (testharness + report hook + META helpers + the test body, all as classic `<script src>`).
/// The origin a test is served from: `KORVUS_WPT_SERVER_HTTPS` for the files WPT marks as needing TLS,
/// the plain server otherwise.
///
/// This mirrors a reference engine's `UrlBuilder::build` exactly, including which markers count. It is the
/// faithful choice rather than the flattering one: serving a `.https.` test over http would let it run
/// and be scored, while sending it to a TLS port our transport may reject makes it produce no verdict
/// and drop out of the denominator. Matching their URL is what "the same test" means, so the cost is
/// taken and reported rather than avoided.
fn origin_for<'a>(path: &str, server: &'a str) -> String {
    let https = path.contains(".https.") || path.contains(".serviceworker.");
    if https {
        if let Ok(h) = std::env::var("KORVUS_WPT_SERVER_HTTPS") {
            if !h.is_empty() {
                return h.trim_end_matches('/').to_string();
            }
        }
        // NO TLS SERVER: fall back to LOOPBACK, not to `server`. A `.https.` file's subject needs a
        // secure context, and `127.0.0.1` is potentially-trustworthy while `web-platform.test` is not
        // -- so once the corpus moved off loopback these fell over. Measured 2026-09-19, three of them
        // by name: `encoding/sharedarraybuffer.https.html`,
        // `fetch/fetch-later/quota/multiple-origins.https.window.js`,
        // `html/cross-origin-opener-policy/resource-popup.https.html`. They had been passing because
        // the plain server HAPPENED to be trustworthy, which is the same accident this whole change
        // exists to remove -- it was simply pointing the other way.
        if let Some(port) = server.rsplit(':').next() {
            return format!("http://127.0.0.1:{port}");
        }
    }
    server.to_string()
}

fn build(root: &Path, tf: &TestFile, server: &str) -> (String, String) {
    match tf.kind {
        Kind::Html => {
            // Read from disk. With an external `wpt serve` this is still the right body for a plain
            // file, and `run_file` replaces it with the server's body in that mode anyway.
            // Bytes, then `decode_document`, NOT `read_to_string`: the latter returns Err for a
            // UTF-16 file and `unwrap_or_default()` would hand the engine an empty document.
            let html = std::fs::read(root.join(&tf.rel)).map(|b| decode_document(&b)).unwrap_or_default();
            // The manifest addresses a plain .html by its own path, so url_path and rel agree here;
            // keeping the field authoritative anyway means one rule, not two.
            let path = tf.url_path.clone().unwrap_or_else(|| tf.rel.clone());
            let base = format!("{}/{}{}", origin_for(&path, server), path, encode_variant(&tf.variant));
            // A `.sub.` document is substituted the way wptserve's FileHandler would have substituted
            // it for this URL. This used to be "a known limit", and it left 377 core test
            // documents holding raw `{{host}}` / `{{GET[...]}}` placeholders. The same function the
            // server uses for its own `.sub.` files, the same one-origin subset (see `wpt_substitute`).
            let html = if tf.rel.contains(".sub.") {
                let ext = tf.rel.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
                let ml = matches!(ext.as_str(), "html" | "htm" | "xht" | "xhtml" | "xml" | "svg");
                String::from_utf8(wpt_substitute(html.as_bytes(), &WptRequest::for_url(&base), ml)).unwrap_or(html)
            } else {
                html
            };
            (html, base)
        }
        Kind::AnyJs => {
            let content = std::fs::read(root.join(&tf.rel)).map(|b| decode_document(&b)).unwrap_or_default();
            let mut metas = String::new();
            for m in meta_scripts(&content, &tf.rel) {
                metas.push_str(&format!("<script src=\"{m}\"></script>\n"));
            }
            // wptserve generates several documents from one `.any.js` -- .any.html, .window.html,
            // .any.worker.html and so on -- and the manifest says WHICH one this test is. Prefer it;
            // fall back to the window-scope name the directory walk assumes.
            let html_path = tf.url_path.clone().unwrap_or_else(|| {
                tf.rel.strip_suffix(".js").map(|s| format!("{s}.html")).unwrap_or_else(|| tf.rel.clone())
            });
            // W1.6: this LOCAL template is a fallback (a real `wpt serve` run overrides it below with the
            // actual generated body); worker-scope shapes must still get the RIGHT fallback shape, because
            // it embedding the `.any.js` body as a window `<script src>` -- correct for `.any.html`/
            // `.window.html` -- would run the test on the wrong global instead of what a real
            // `.any.worker.html`/`.any.sharedworker.html` wrapper does: `fetch_tests_from_worker(new
            // Worker(<sibling .js>))`. The sibling worker script IS the compiled `.any.js`
            // (wptserve names it `<stem>.any.worker.js`, not the html path), which is why the URL swaps
            // ".html" for ".js" rather than pointing at `tf.rel`.
            // wptserve compiles ONE worker script per `.any.js` and names it `<stem>.any.worker.js`, whatever
            // scope the document is for -- `foo.any.sharedworker.html` and `foo.any.serviceworker.html` both
            // load `foo.any.worker.js`. Deriving the name by swapping `.html` for `.js` invents
            // `foo.any.serviceworker.js`, which 404s, and a worker whose script 404s reports nothing at all.
            // (The `.sharedworker` form was wrong the same way; it never showed because with a live server
            // that document's body comes from `http_get`, and only the `.serviceworker.` ones -- routed to
            // the TLS origin, which `http_get` cannot speak -- actually fall back to this template.)
            let worker_js = html_path
                .strip_suffix(".any.sharedworker.html")
                .or_else(|| html_path.strip_suffix(".any.serviceworker.html"))
                .map(|s| format!("{s}.any.worker.js"))
                .or_else(|| html_path.strip_suffix(".html").map(|s| format!("{s}.js")))
                .unwrap_or_else(|| tf.rel.clone());
            // A `.any.serviceworker.html` wrapper is not a constructor call: wptserve generates a
            // register()-then-`fetch_tests_from_worker(reg.installing)` document, and `new Worker(...)`
            // here would run a SERVICE-worker test in a DEDICATED worker -- the wrong global, which is
            // exactly what this file refuses to do. This fallback is load-bearing rather than academic:
            // `http_get` speaks plain HTTP only, and `origin_for` sends every `.serviceworker.` URL to
            // the TLS origin, so the server body is never fetched for these and the template below is
            // what actually runs.
            let html = if html_path.ends_with(".any.serviceworker.html") {
                format!(
                    "<!doctype html>\n<meta charset=\"utf-8\">\n<title>{rel}</title>\n<div id=\"log\"></div>\n\
                     <script src=\"/resources/testharness.js\"></script>\n\
                     <script src=\"/resources/testharnessreport.js\"></script>\n\
                     {metas}<script>\n(async function() {{\n\
                     \x20 const scope = 'does/not/exist';\n\
                     \x20 let reg = await navigator.serviceWorker.getRegistration(scope);\n\
                     \x20 if (reg) await reg.unregister();\n\
                     \x20 reg = await navigator.serviceWorker.register(\"/{worker_js}\", {{scope}});\n\
                     \x20 fetch_tests_from_worker(reg.installing);\n\
                     }})();\n</script>\n",
                    rel = tf.rel,
                )
            } else if worker_scope_actually_runs(&html_path) {
                let ctor = if html_path.ends_with(".any.sharedworker.html") { "SharedWorker" } else { "Worker" };
                format!(
                    "<!doctype html>\n<meta charset=\"utf-8\">\n<title>{rel}</title>\n<div id=\"log\"></div>\n\
                     <script src=\"/resources/testharness.js\"></script>\n\
                     <script src=\"/resources/testharnessreport.js\"></script>\n\
                     {metas}<script>fetch_tests_from_worker(new {ctor}(\"/{worker_js}\"));</script>\n",
                    rel = tf.rel,
                )
            } else {
                format!(
                    "<!doctype html>\n<meta charset=\"utf-8\">\n<title>{rel}</title>\n<div id=\"log\"></div>\n\
                     <script src=\"/resources/testharness.js\"></script>\n\
                     <script src=\"/resources/testharnessreport.js\"></script>\n\
                     {metas}<script src=\"/{rel}\"></script>\n",
                    rel = tf.rel,
                )
            };
            let base = format!("{}/{html_path}{}", origin_for(&html_path, server), encode_variant(&tf.variant));
            (html, base)
        }
    }
}

struct Outcome {
    passed: usize,
    total: usize,
    /// harness status: 0=OK, 1=ERROR, 2=TIMEOUT, -1=did-not-complete (partial capture used).
    harness: i64,
    completed: bool,
    /// testharness.js's `test` global was present after load (real harness fetched + ran).
    harness_loaded: bool,
    failures: Vec<(String, i64, String)>, // (subtest name, status, message)
    /// Uncaught errors and unhandled rejections the page raised. The diagnostic for a file that
    /// produced no verdict at all, where there is otherwise nothing but the path.
    errors: Vec<String>,
    /// EVERY subtest's (name, status), passing ones included. The failures list alone cannot support
    /// per-member attribution: a file named `Interface-member` that is 19/20 has to be readable as
    /// "these 19 assertions passed" rather than collapsing to one all-or-nothing verdict for the file.
    subtests: Vec<(String, i64)>,
    /// OBSERVATION ONLY (see `hit_pump_budget`'s doc comment) -- how many tests testharness.js had
    /// STARTED (`add_test_state_callback`, fires on test entry, before a result) when time ran out.
    /// Distinguishes "registered a lot, almost none finished" from "registered almost nothing at all"
    /// for a file that never reaches `total > 0` -- the same information count as `subtests.len()` for
    /// a file that DOES reach a verdict, but for one that does not, `subtests` is always empty.
    tests_registered: usize,
    /// OBSERVATION ONLY: the pump loop's OWN deadline (`budget`) elapsed before `__wptresults` was set.
    /// Never influences `passed`/`total`/`completed` -- read after the scoring decision is already made,
    /// so a sweep with this field on scores every file identically to one with it off.
    hit_pump_budget: bool,
    /// OBSERVATION ONLY: the interfaces whose members this file called, when KORVUS_TOUCH_LOG is set
    /// (the impact map). Filled by the sweep, not by run_file.
    touched: Vec<String>,
    /// OBSERVATION ONLY: the watchdog thread actually called `IsolateInterrupt::terminate()` on this
    /// file (as opposed to standing down because the file finished first). Same non-scoring guarantee as
    /// `hit_pump_budget`.
    hit_watchdog: bool,
    /// OBSERVATION ONLY: how many times the watchdog had to call `terminate()` on this file. `0` is the
    /// normal file (it stood down); `1` is a file that hung once and stayed dead; **`>= 2` is a file
    /// that came back to life after `clear_termination()` and had to be killed again** -- which is the
    /// signature of the defect this counter was added for (see the watchdog's block comment).
    watchdog_terminations: u32,
}

// `KORVUS_WPT_a reference engine=1` NO LONGER CHANGES SCORING. The variable is still set by
// scripts/wpt-tiers.sh; the runner ignores
// it, and the `fn a reference engine_accounting()` that read it is gone.
//
// What it used to do, and why that was wrong. a reference engine's runner records a file that never published a
// result as `subtests: Vec::new()` with `total() == subtests.len()`, so such a file contributes to
// neither side of their ratio. This flag made us reproduce that by SKIPPING the runner's force-
// finalisation step -- and the two are not the same event. Theirs is "the page published nothing".
// Ours was "we stopped pumping the page after 1.2 seconds", on a page whose own auto-timeout our
// report hook had disabled. The result was 190 core files that had declared real subtests silently
// leaving the denominator, and a headline pass rate that rose because tests disappeared from it.
//
// The runner now forces `timeout()` for every unfinished file (see the long comment at the call site),
// which is what testharness.js's own timer does for a reference engine inside their 15s budget. The remaining
// difference between the two runs is therefore the BUDGET alone -- ours ~2.5s/7.5s against their 15s
// -- not the accounting, and a budget difference is a thing the summary can state in one line instead
// of a rule buried in a branch.

async fn run_file(server: &str, root: &Path, tf: &TestFile) -> Outcome {
    // A worker-scope URL W1.6 gives no real load-and-run path to (ServiceWorker registration is
    // source-only) has no window harness to run in. Under a reference engine's rule a test with no result is 0/0,
    // which is exactly what a runner without that path gets -- so report it that way rather than letting
    // it hang on a registration/fetch that never arrives. `.any.worker.html`/`.any.sharedworker.html` run
    // for real below (see `worker_scope_actually_runs`).
    let mut is_worker = false;
    if let Some(u) = &tf.url_path {
        if is_worker_scope(u) && !worker_scope_actually_runs(u) {
            return Outcome { passed: 0, total: 0, harness: -1, completed: false, harness_loaded: false, failures: Vec::new(), errors: Vec::new(), subtests: Vec::new(), tests_registered: 0, hit_pump_budget: false, hit_watchdog: false, watchdog_terminations: 0, touched: Vec::new() };
        }
        is_worker = worker_scope_actually_runs(u);
    }
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap(); // boot (Korvus bootstrap is well-formed)

    let (html, base) = build(root, tf, server);
    // Against a real wpt serve, take the body from the server. See http_get: the on-disk bytes are the
    // wrong body for everything wptserve generates or substitutes, and those are exactly the files
    // that could not reach a verdict before.
    let mut doc_headers: Vec<(String, String)> = Vec::new();
    let html = if std::env::var("KORVUS_WPT_SERVER").map(|v| !v.is_empty()).unwrap_or(false) {
        match http_get(&base).await {
            Some((b, h)) if !b.trim().is_empty() => {
                doc_headers = h;
                b
            }
            _ => html,
        }
    } else {
        html
    };
    // Body from disk means headers from the sidecar (`http_get` is plain-HTTP-only, so every
    // `.https.` document lands here). Never both: a response that really came off the wire already
    // has its own.
    if doc_headers.is_empty() {
        doc_headers = sidecar_headers(root, &tf.rel, &base);
    }

    // Both budgets are decided HERE, not before the fetch, because `<meta name=timeout content=long>`
    // is a property of the DOCUMENT -- and for a `.any.js` the meta only exists in the wrapper wptserve
    // generated, which is the body we just fetched.
    let long = wants_long_timeout(&html);
    let mult = if long { LONG_TIMEOUT_MULTIPLIER } else { 1 };
    let deadline_ms = global_deadline_override()
        .unwrap_or((if is_worker { WORKER_HARD_DEADLINE_MS } else { HARD_DEADLINE_MS }) * mult);

    // Watchdog: a page script (or a testharness path) can spin V8 synchronously without ever yielding to
    // the event loop, which the cooperative `tokio::time::timeout` around the async pump cannot bound (a
    // Rust timer only fires at an await point; V8 running synchronous JS never reaches one). Hand a
    // thread-safe isolate handle to an OS thread that calls terminate() at HARD_DEADLINE_MS to break such
    // a loop; it stands down (via `finished`) the instant the file completes normally. Once terminate has
    // fired, V8 fail-fasts every later script until clear_termination(), so the readback below still
    // scores the file (as did-not-complete) instead of hanging. Belt-and-suspenders behind the arena's
    // own insertion cycle-guard, which removes the currently-known hang at the source.
    let interrupt = e.isolate_interrupt();
    let finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let wd_finished = finished.clone();
    // OBSERVATION ONLY, per the peer session's instrumentation ask: counts the terminate() calls the
    // watchdog actually made (0 = it stood down because the file finished). Read back below, never fed
    // into scoring. `Outcome::hit_watchdog` / `Outcome::watchdog_terminations`.
    let watchdog_kills = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let wd_kills = watchdog_kills.clone();
    // THE WATCHDOG GUARDS THE WHOLE FILE, NOT ONLY THE PUMP LOOP.
    //
    // It used to fire once at `deadline_ms` and then be joined immediately after the pump loop broke --
    // BEFORE the runner called `clear_termination()` and resumed the isolate to finalise the file
    // (`timeout()`, then `pump(300ms)`, then the result read-backs). So every one of those steps ran on
    // a revived isolate with NO watchdog left, and `pump()`'s own `tokio::time::timeout` cannot bound
    // synchronous JS -- the exact thing the watchdog exists for. A page that is still looping when it is
    // revived therefore loops forever, with the guard already retired.
    //
    // Measured on `IndexedDB/event-dispatch-active-flag.any.js`, whose `keep_alive()` spin re-arms an
    // IDB request from its own success handler: two runs of the identical binary and command, one
    // bounded at 8,647 ms and one still growing at 13 MB/s when a 90 s / 2 GB cap killed it at 1,585 MB.
    // The difference is only WHERE the single terminate() landed -- inside the microtask drain (which
    // V8 then discards, so the file stays dead) or between drains (so the queued drain survives, and
    // `clear_termination()` hands it straight back to the loop). A one-shot kill cannot cover that race.
    //
    // So the thread now stays alive for the whole of `run_file`: after the first kill it allows
    // `FINALIZE_GRACE_MS` for finalisation -- generous for four evals and a 300 ms pump, far short of a
    // live loop -- and then kills again, every 50 ms, until `run_file` sets `finished`. `run_file` calls
    // `clear_termination()` a second time after the join, so the read-backs still run on a live isolate
    // and a terminated file is still scored from whatever settled before the hang.
    //
    // WHAT THIS WATCHDOG CANNOT DO, MEASURED. `terminate()` unwinds JAVASCRIPT frames.
    // A loop inside a native Web API member callback has none, so the interrupt is never consulted
    // and the thread above is powerless however often it fires. Both halves were measured on
    // `html/select/options-length-too-large.html`, whose first statement -- `options.length = -1`,
    // i.e. `ToUint32(-1)` = 4,294,967,295 -- used to run an unbounded Rust `for` loop appending
    // `<option>` elements:
    //
    //     a JavaScript `while (true) {}`   broken by the FIRST terminate()  (2,000 ms fuse, 1 kill)
    //     that native loop                 400 terminate() calls over 20 s, never preempted;
    //                                      killed at 120 s with the loop still going
    //
    // So shard 2/11's "hang" was never a watchdog that failed to fire, and never a loop outside the
    // watched window: it is a class of hang this mechanism cannot reach. `HARD_DEADLINE_MS` bounds a
    // page that loops in JS; only `KORVUS_WPT_SHARD_TIMEOUT` (30 min, scripts/wpt-tiers.sh) bounds
    // one that loops in a member's own implementation, and it costs the whole shard. The fix for
    // such a defect therefore belongs at the member -- that one is `OPTIONS_LENGTH_CAP` in
    // korvus-webapi's `collections.rs`, and `tests/options_length.rs` pins it against Chrome.
    let watchdog = std::thread::spawn(move || {
        let mut waited: u64 = 0;
        while waited < deadline_ms {
            std::thread::sleep(Duration::from_millis(50));
            if wd_finished.load(std::sync::atomic::Ordering::Relaxed) { return; }
            waited += 50;
        }
        wd_kills.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        interrupt.terminate();
        let mut grace: u64 = 0;
        while grace < FINALIZE_GRACE_MS {
            std::thread::sleep(Duration::from_millis(50));
            if wd_finished.load(std::sync::atomic::Ordering::Relaxed) { return; }
            grace += 50;
        }
        while !wd_finished.load(std::sync::atomic::Ordering::Relaxed) {
            wd_kills.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            interrupt.terminate();
            std::thread::sleep(Duration::from_millis(50));
        }
    });

    // load_page fetches testharness.js / the report hook / helpers / the test body over transport and
    // runs them in document order, then dispatches DOMContentLoaded + load (load on a 0ms task).
    // Bounded: a file whose subresource fetch never settles (missing/hanging helper) must not wedge the
    // whole suite — on timeout it just scores low, and the run continues.
    let _ = tokio::time::timeout(Duration::from_secs(6), e.load_page_delivered(&html, &base, &[], &doc_headers)).await;

    // Probe `add_completion_callback`, NOT `test`.
    //
    // This is a REPORTING fix and recovers no verdict by itself -- it moves files between the
    // diagnosis buckets so the ones left in `harness-never-loaded` are really files where the harness
    // never loaded. `typeof globalThis.test === 'function'` asks the page a question the page is
    // allowed to answer wrongly: 19 core files open with `var test = async_test();` at global scope
    // (xhr/event-progress.any.js:4, xhr/send-sync-blocks-async.htm:13, and 17 more), which replaces the
    // global `test` FUNCTION with a Test OBJECT. testharness.js had loaded and run perfectly; we
    // recorded `harness_loaded: false` and filed them under "testharness.js never ran".
    //
    // `add_completion_callback` is a harness-internal registration API no test has any reason to
    // shadow, and our own report hook has already called it by this point, so its absence really does
    // mean the harness is not there.
    let harness_loaded = e
        .eval_value("wpt:probe", "typeof globalThis.add_completion_callback === 'function'".to_string())
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Pump in short slices until the harness reports completion (globalThis.__wptresults set) -- NOT a
    // single long pump. Korvus keeps a perpetual timer alive (like a real page), so run_event_loop never
    // fully drains and one pump(budget) would always cost its whole budget even for a file that completed
    // in the first millisecond (measured: harness=OK files still burned the full 2s). Slicing + a
    // completion probe lets a completing file return in ~one slice; only a file with a genuinely stuck
    // async subtest spends the whole budget. The load task (dispatched by load_page) fires in slice 1.
    // Uniform for every file, worker-scope included -- see WORKER_HARD_DEADLINE_MS's doc comment for why
    // a wider worker-scope budget is NOT here: tried, measured to cost 2.7x the sweep's wall-clock for no
    // gain in worker-scope verdict coverage. `global_deadline_override` (KORVUS_WPT_DEADLINE_MS), when
    // set, widens this for EVERY file at once, leaving margin below the watchdog's own `deadline_ms` so
    // the pump loop's deadline is what ends the file rather than a race with terminate().
    let budget = match global_deadline_override() {
        Some(_) => Duration::from_millis(deadline_ms.saturating_sub(500)),
        None => Duration::from_millis(PUMP_BUDGET_MS * mult),
    };
    let slice = Duration::from_millis(50);
    let deadline = std::time::Instant::now() + budget;
    let mut hit_pump_budget = false;
    loop {
        let end = e.pump(slice).await;
        let done = e
            .eval_value("wpt:done?", "typeof globalThis.__wptresults === 'string'".to_string())
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if done {
            break;
        }
        // A DRAINED loop has no timer, no in-flight fetch and no unresolved promise left, so the
        // harness cannot publish `__wptresults` however long we wait. This is the no-verdict file --
        // 39.7% of the corpus -- and before this check every one of them spun a core for its whole
        // budget to arrive at the answer it already had. Breaking here does not change the verdict
        // (the file still falls to the force-timeout path below); it only stops paying for it, and
        // it keeps `hit_pump_budget` false, which is the honest reading: the budget is not what
        // ended this file.
        if matches!(end, Ok(engine::PumpEnd::Drained)) {
            break;
        }
        if std::time::Instant::now() >= deadline {
            // OBSERVATION ONLY: this file did not finish on its own; the PUMP LOOP's deadline (not the
            // watchdog, which is 4000ms and independent) is what ended it. `Outcome::hit_pump_budget`.
            hit_pump_budget = true;
            break;
        }
    }

    // Clear any termination the watchdog posted, so finalization + the result-readback evals below run
    // on a live isolate. Normal file: the watchdog never fired and clear_termination() is a no-op. Hung
    // file: the pump loop above spun through fail-fast errors to its deadline, and we now unwind the
    // poison so the file is scored from whatever subtests settled before the break.
    //
    // THE WATCHDOG IS NOT STOOD DOWN HERE. It used to be -- joined on this line, before the isolate was
    // revived -- which left everything below unguarded and let a revived loop run forever (the block
    // comment where the thread is spawned carries the measurement). It is now holding a
    // `FINALIZE_GRACE_MS` window over exactly the steps that follow, and is stood down after them.
    e.clear_termination();

    // OBSERVATION ONLY: how many tests were registered at the moment the pump loop stopped (naturally or
    // via its deadline) -- read HERE, before the force-timeout finalization below can synthesize
    // completions, so it reflects what the harness had actually reached, not what force-timeout added.
    let tests_registered = e
        .eval_value("wpt:registered", "globalThis.__wptregistered ?? 0".to_string())
        .ok()
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;

    // If the completion callback never fired (a still-pending async subtest), force finalization so that
    // subtest is recorded as a failure instead of blackholing the whole file, then settle briefly.
    let completed_now = e
        .eval_value("wpt:done?", "typeof globalThis.__wptresults === 'string'".to_string())
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    // Force finalisation ALWAYS, a reference engine accounting included. The `&& !a reference engine_accounting()` that used
    // to guard this line was the single largest source of invisible no-verdict files, and removing it
    // LOWERS our headline number on purpose.
    //
    // How the two pieces combined into a hole. Our report hook calls `setup({explicit_timeout: true})`,
    // which sets `tests.timeout_length = null` and so cancels testharness.js's OWN auto-timeout -- the
    // 10s (60s with `meta timeout=long`) timer that would otherwise finalise a stuck file by itself.
    // That is a deliberate trade: the runner owns finalisation deterministically instead of racing a
    // timer. But it only works if the runner actually finalises, and this branch was the runner's only
    // way to do it. Gated off under `KORVUS_WPT_a reference engine=1` -- which is the flag the like-for-like sweep
    // runs with -- nothing finalised: `__wptresults` was never set, `subtests` stayed empty, and the
    // file left the denominator entirely at 0/0 instead of landing in it at 0/N.
    //
    // The gate was reasoning by analogy from their code and got the analogy backwards. a reference engine records
    // `subtests: Vec::new()` for a file that published NOTHING, and we copied the shape of that
    // outcome. But in their runs most such files do publish: their overlay
    // (a reference implementation) sets only
    // `{output: false}` and never `explicit_timeout`, so testharness's own 10s timer survives, fires
    // inside their 15s per-test budget, and the file comes back as a scored TIMEOUT that LOWERS their
    // pass rate. We were mirroring the rare case (nothing published) onto the common one (we cut the
    // page off early), which is not the same event at all.
    //
    // WHAT THIS RUNNER NOW DOES vs THEIRS, for the like-for-like record:
    //   ours    ~2.5s pump (7.5s for `meta timeout=long`), then an explicit `timeout()`; every file
    //           that declared at least one subtest is scored 0/N, whatever its meta says.
    //   theirs  15s budget, testharness's own timer: a normal file self-times-out at 10s and is scored
    //           0/N; a `meta timeout=long` file's timer is 60s, never fires inside 15s, and IS recorded
    //           0/0 by them.
    // So on `timeout=long` files we are now STRICTER than a reference engine -- we put in the denominator what
    // they drop. That direction costs us pass rate rather than earning it, which is why it is the
    // acceptable side to err on; it is a real remaining difference and not a wash.
    //
    // A file that registered ZERO subtests is unaffected: `timeout()` finalises it with an empty
    // `tests` array, so it is still 0/0 and still outside the ratio. This branch converts exactly the
    // files that declared work and were stopped before finishing it.
    if !completed_now {
        let _ = e.eval_value("wpt:force-timeout", FORCE_TIMEOUT.to_string());
        let _ = e.pump(Duration::from_millis(300)).await;
    }

    // NOW stand the watchdog down: everything that can execute page JS is behind us. A file the watchdog
    // had to kill a second time reaches this line with a termination posted, so clear it again -- the
    // two read-backs below are property reads that cannot loop, and without this they would return None
    // and throw away the subtests that DID settle before the hang.
    finished.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = watchdog.join();
    e.clear_termination();
    let watchdog_terminations = watchdog_kills.load(std::sync::atomic::Ordering::Relaxed);
    let hit_watchdog = watchdog_terminations > 0;

    // Page errors, read back before the results. For a file that reaches no verdict this is the ONLY
    // evidence of why: the harness loaded, something threw during setup, and without the message there
    // is nothing to act on but a filename.
    let errors: Vec<String> = e
        .eval_value("wpt:errors", "JSON.stringify(globalThis.__wpterrors || [])".to_string())
        .ok()
        .and_then(|v| v.as_str().and_then(|s| serde_json::from_str::<Vec<String>>(s).ok()))
        .unwrap_or_default();

    let results = e
        .eval_value("wpt:results", "globalThis.__wptresults ?? null".to_string())
        .unwrap_or(serde_json::Value::Null);
    let parsed: Option<serde_json::Value> = results.as_str().and_then(|s| serde_json::from_str(s).ok());

    let (harness, completed, subtests): (i64, bool, Vec<serde_json::Value>) = match parsed {
        Some(v) => (
            v["harness"].as_i64().unwrap_or(-1),
            true,
            v["tests"].as_array().cloned().unwrap_or_default(),
        ),
        None => {
            let partial = e
                .eval_value("wpt:partial", "globalThis.__wptpartial ?? []".to_string())
                .unwrap_or(serde_json::Value::Array(Vec::new()));
            (-1, false, partial.as_array().cloned().unwrap_or_default())
        }
    };

    let mut passed = 0usize;
    let mut failures: Vec<(String, i64, String)> = Vec::new();
    let mut all: Vec<(String, i64)> = Vec::new();
    for t in &subtests {
        let name = t["name"].as_str().unwrap_or("<unnamed>").to_string();
        let status = t["status"].as_i64().unwrap_or(-1);
        let message = t["message"].as_str().unwrap_or("").to_string();
        all.push((name.clone(), status));
        if status == 0 {
            passed += 1;
        } else {
            failures.push((name, status, message));
        }
    }

    Outcome { passed, total: subtests.len(), harness, completed, harness_loaded, failures, errors, subtests: all, tests_registered, hit_pump_budget, hit_watchdog, watchdog_terminations, touched: Vec::new() }
}

/// THE HARNESS BOUND, demonstrated on a file built to defeat it.
///
/// This is a gate test, not a scoreboard one: a WPT file that never stops running must cost the runner
/// a bounded amount of wall clock, because `gate.sh --full` runs the scoreboard and an unbounded file
/// takes the machine with it (measured 2026-09-11: 54.8 GB RSS, orphan processes, a 90 s / 2 GB capped
/// reproduction still climbing at 13 MB/s when the cap fired).
///
/// The fixture hangs in BOTH phases, because the runner has two and the watchdog used to guard only
/// the first:
///
/// * **the pump phase** — `function spin(){ queueMicrotask(spin); } spin();` is an infinite MICROTASK
///   chain. V8 drains microtasks to exhaustion before returning to the task queue, so this never yields:
///   the `setTimeout(…, 0)` beside it never gets a turn, `pump()`'s cooperative `tokio::time::timeout`
///   never reaches an await point, and only `terminate()` from another thread can end it. (It is the
///   shape `IndexedDB/resources/support.js`'s `keep_alive()` produces through an IDB request chain,
///   reduced to three lines.)
/// * **the finalisation phase** — an `add_completion_callback` that loops forever. The runner revives
///   the isolate with `clear_termination()` and then calls `timeout()`, whose `notify_complete` runs
///   the completion callbacks SYNCHRONOUSLY. Before this unit the watchdog had already been joined by
///   then, so this loop ran with no guard at all and the process never returned.
///
/// The assertion is the bound AND the second kill: `watchdog_terminations >= 2` is what says the guard
/// really extends past the revival, and would go back to 1 the moment someone re-joins the watchdog
/// before finalisation.
#[tokio::test]
async fn watchdog_bounds_a_file_that_hangs_in_every_phase() {
    let dir = std::env::temp_dir().join(format!("korvus-wpt-hang-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp corpus root");
    // The corpus root only has to answer `/resources/testharness.js`; the report hook is served from
    // the vendored fixture whatever the root is (see `serve`).
    std::fs::create_dir_all(dir.join("resources")).expect("resources dir");
    std::fs::copy(
        vendored_wpt_root().join("resources/testharness.js"),
        dir.join("resources/testharness.js"),
    )
    .expect("copy testharness.js");
    std::fs::write(
        dir.join("hang.html"),
        r#"<!doctype html>
<meta charset="utf-8">
<title>deliberately unbounded</title>
<script src="/resources/testharness.js"></script>
<script src="/resources/testharnessreport.js"></script>
<script>
add_completion_callback(function () { for (;;) {} });
async_test(function (t) {
  setTimeout(t.step_func_done(function () {}), 0);
  function spin() { queueMicrotask(spin); }
  spin();
}, 'a microtask chain that never yields to the task queue');
</script>
"#,
    )
    .expect("write hang.html");

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let server = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(serve(listener, dir.clone()));

    let tf = TestFile {
        rel: "hang.html".to_string(),
        kind: Kind::Html,
        area: "harness".to_string(),
        variant: String::new(),
        url_path: None,
    };
    let t0 = std::time::Instant::now();
    let o = run_file(&server, &dir, &tf).await;
    let elapsed = t0.elapsed();
    let _ = std::fs::remove_dir_all(&dir);

    // The budget the runner promises: one hard deadline, one finalisation grace, and slack for the
    // subresource fetches and the 50 ms watchdog tick. Anything past this is an unbounded file.
    let ceiling = Duration::from_millis(HARD_DEADLINE_MS + FINALIZE_GRACE_MS + 12_000);
    assert!(
        elapsed < ceiling,
        "a file that never stops running must still be bounded: took {elapsed:?}, ceiling {ceiling:?}"
    );
    assert!(o.hit_watchdog, "the watchdog must have fired on a file that never stops running");
    assert!(
        o.watchdog_terminations >= 2,
        "the watchdog must still be armed after clear_termination() revives the isolate for \
         finalisation -- a single kill means it was joined too early ({} termination(s))",
        o.watchdog_terminations
    );
}

// Broad WPT corpus (dozens of real files, each re-fetching + re-parsing the 198KB testharness.js over
// load_page) — several minutes, too heavy for the per-commit gate. Opt-in:
//   cargo test -p korvus-runtime --test wpt -- --ignored --nocapture
// The fast per-commit capability signal is obstacle_course; this is the periodic conformance scoreboard.
#[ignore = "heavy broad-corpus WPT scoreboard; run explicitly with --ignored"]
#[tokio::test]
async fn wpt_conformance_scoreboard() {
    let root = wpt_root();

    // ANNOUNCE THE CORPUS BEFORE MEASURING IT. `wpt_root()` reads `KORVUS_WPT_ROOT` out of the AMBIENT
    // environment, and until 2026-09-11 nothing printed the value it resolved -- so a run's own output
    // could not tell you whether it had measured the 84-file vendored fixture or a 69,856-file upstream
    // checkout. That cost a day: a `gate.sh --full` run reached 54.8 GB RSS over 38 minutes, the same
    // commit and the same command run deliberately took 18 s / 126 MB, and because neither run's output
    // named its corpus there was no way to tell the two apart after the fact. The root is printed HERE,
    // before the server bind and the walk, so a run that dies inside the walk still says what it was
    // walking; the file counts follow the walk (see the `wpt corpus:` line below).
    eprintln!(
        "wpt corpus root: {} [{}]",
        root.display(),
        match std::env::var("KORVUS_WPT_ROOT") {
            Ok(p) if !p.is_empty() => "KORVUS_WPT_ROOT",
            _ => "vendored fixture (KORVUS_WPT_ROOT unset)",
        }
    );

    // `KORVUS_WPT_SERVER` points at a real `wpt serve` instead of the built-in static server.
    //
    // The static server is a path-router over the checkout: it satisfies `<script src>` and nothing
    // else. WPT is not only files. A large part of the suite needs the real server -- `.sub.` files
    // whose contents are substituted at request time, `.py` handlers that compose responses, the
    // subdomain origins cross-origin tests navigate to, and https. Serving those as static bytes
    // hands the page unsubstituted placeholders, so the test cannot reach a verdict. That is most of
    // the 37% of core URLs that produced no verdict at all under a reference engine's accounting -- a property
    // of our harness, not of the engine, and the reason this switch exists.
    //
    // With an external server the report hook cannot be injected by rewriting a route: the real
    // wptserve returns WPT's own testharnessreport.js, which reports to wptrunner and not to us. It
    // has to be installed INTO the checkout, over resources/testharnessreport.js -- which is exactly
    // what the reference benchmark's setup-wpt.sh does with its overlay.
    let external = std::env::var("KORVUS_WPT_SERVER").ok().filter(|s| !s.is_empty());
    let server = match &external {
        Some(base) => base.trim_end_matches('/').to_string(),
        None => {
            // ON ITS OWN OS THREAD. A synchronous XHR blocks the thread the engine runs
            // on until its response arrives, and a server spawned onto this test's runtime is polled
            // by that same thread: every `open(m, u, false)` in the corpus deadlocked against its own
            // server until the sync-fetch bound (measured: xhr/sync-xhr-and-window-onload.html sat
            // 15 s and errored). A browser's server is never its own event loop.
            let std_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind wpt static server");
            let addr = std_listener.local_addr().unwrap();
            std_listener.set_nonblocking(true).expect("nonblocking listener");
            // THE ORIGIN'S TRUSTWORTHINESS IS PART OF WHAT A TEST MEASURES, and `127.0.0.1` is
            // potentially-trustworthy, so every file runs in a SECURE context here. WPT itself serves
            // from `web-platform.test` precisely so that it is not. Measured 2026-09-19:
            // `fetch/fetch-later/non-secure.window.js`, whose whole subject is the non-secure case,
            // PASSES at `web-platform.test` and FAILS at `127.0.0.1` on the same binary and the same
            // runner -- and real Chrome would fail it here too. It had been passing for the wrong
            // reason: korvus had no `fetchLater` at all, and it broke when korvus became MORE correct.
            //
            // `KORVUS_WPT_SERVER_HOST` substitutes the hostname in the URL only; the socket stays on
            // 127.0.0.1, since the name resolves there. Default is unchanged until the blast radius
            // over ~5,500 files is measured both ways rather than asserted.
            // DEFAULT IS `web-platform.test`, which is what upstream WPT serves from and the reason
            // its layout needs no list of "which files are about the insecure case". Measured both
            // ways over the whole core tier, 5,789 files, with every flipped file re-run ALONE so
            // contention could not forge the difference: 12 files pass ONLY off loopback's
            // trustworthiness being absent -- cors/script-304, two fetch redirect files, two
            // cross-origin-resource-policy files, fetch-later/non-secure, COOP no-https,
            // base_href_specified, access-control-expose-headers-on-redirect, both
            // event-upload-progress files, xhr/timeout-multiple-fetches -- and ZERO are lost once
            // `.https.` files keep a loopback origin (see `origin_for`). Two more moved and did not
            // survive the solo re-check; they were load, not origin.
            //
            // The name must RESOLVE, and it is in `/etc/hosts` here only because a WPT checkout was
            // set up. If it does not, say so loudly and stay on the IP rather than failing 5,789
            // files for a hosts entry -- a tier that silently measures a different question is worse
            // than one that says it could not.
            let host = std::env::var("KORVUS_WPT_SERVER_HOST").unwrap_or_default();
            let host = if host.is_empty() { "web-platform.test".to_string() } else { host };
            let resolves = std::net::ToSocketAddrs::to_socket_addrs(&(host.as_str(), addr.port()))
                .map(|mut it| it.next().is_some())
                .unwrap_or(false);
            let s = if resolves {
                format!("http://{}:{}", host, addr.port())
            } else {
                eprintln!("wpt: {host:?} does not resolve -- serving from {addr} instead. Every file \
                           will run in a POTENTIALLY-TRUSTWORTHY context, so any test whose subject is \
                           the insecure case will pass for the wrong reason. Add `127.0.0.1 {host}` to \
                           /etc/hosts.");
                format!("http://{addr}")
            };
            let serve_root = root.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("wpt server runtime");
                rt.block_on(async move {
                    let listener = TcpListener::from_std(std_listener).expect("tokio listener");
                    serve(listener, serve_root).await;
                });
            });
            s
        }
    };

    // WPT's WebSocket servers (and WPT's CA for `wss`), before the warm-up below builds the first
    // transport client: `KORVUS_EXTRA_CA` is read when a client is built.
    if external.is_none() {
        init_ws_servers(&root);
    }

    // Warm the transport's process-global TLS trust store before any measured file. wreq lazily
    // initializes its cert store on the first fetch, and in a debug build that init enumerates a large
    // system cert directory (getdirentries) — a one-time cost that otherwise lands entirely on whichever
    // file fetches first (measured: ~19s on the alphabetically-first file, the rest ~130ms), inflating it
    // and tripping the per-file watchdog into a false TERMINATED. A throwaway warmup fetch here moves that
    // cost to setup so every scored file starts with the store already resident. (The underlying slow
    // cert-dir scan is a separate transport finding — a compiled-in root bundle would avoid it entirely.)
    {
        let mut warm = Engine::new(Box::new(persona::CapturedPersona::default()));
        warm.run().await.unwrap();
        let warm_html = "<!doctype html><script src=\"/resources/testharness.js\"></script>";
        let _ = tokio::time::timeout(Duration::from_secs(40), warm.load_page(warm_html, &format!("{server}/__warmup.html"), &[])).await;
    }

    // Discover the corpus (sorted for a stable scoreboard). `KORVUS_WPT_DIRS` narrows the walk to
    // named subtrees — required when `KORVUS_WPT_ROOT` is a full upstream checkout, where walking
    // everything would enumerate ~26k files.
    let mut files: Vec<TestFile> = Vec::new();
    // KORVUS_WPT_MANIFEST switches to WPT's canonical list. Anything the manifest yields is kept as
    // it stands: no iframe filter, no worker-only filter, no custom-elements-helper filter. The point
    // of the mode is to run THEIR set, and a set we pruned is not theirs.
    if let Ok(lp) = std::env::var("KORVUS_WPT_LIST") {
        // Pre-expanded and PRE-SHARDED by the driver: take it as given, no filtering and no re-sharding.
        discover_from_list(Path::new(&lp), &mut files);
        eprintln!("list mode: {} test URLs", files.len());
    } else if let Ok(mp) = std::env::var("KORVUS_WPT_MANIFEST") {
        discover_from_manifest(Path::new(&mp), &mut files);
        let dirs = wpt_dirs();
        if !dirs.is_empty() {
            files.retain(|f| dirs.iter().any(|d| f.rel == *d || f.rel.starts_with(&format!("{d}/"))));
        }
        let skip = excluded_prefixes();
        if !skip.is_empty() {
            files.retain(|f| !skip.iter().any(|p| f.rel == *p || f.rel.starts_with(&format!("{p}/"))));
        }
        eprintln!("manifest mode: {} test URLs", files.len());
    } else {
    let dirs = wpt_dirs();
    if dirs.is_empty() {
        discover(&root, &root, &mut files);
    } else {
        for d in &dirs {
            let sub = root.join(d);
            if sub.is_dir() {
                discover(&root, &sub, &mut files);
            } else {
                eprintln!("KORVUS_WPT_DIRS: no such subtree, skipping: {}", sub.display());
            }
        }
    }
    }
    files.sort_by(|a, b| a.id().cmp(&b.id()));
    // The number the root line above could not know yet: how many test URLs the walk actually found.
    // Printed BEFORE sharding and the filters, so it is the size of the corpus itself and not of this
    // shard's slice of it; the `selected` line after the filters reports what this process will run.
    let discovered = files.len();

    // Shard at TEST-URL granularity (KORVUS_WPT_SHARD=<i>/<n>): keep every n-th URL starting at i.
    // The whole corpus is discovered by every shard -- that costs a second of file reads and buys the
    // only property that matters here, which is balance.
    //
    // The obvious alternative, giving each process its own subtree, is what the first parallel run did
    // and it is badly wrong: cost per file spans four orders of magnitude (p50 149ms, but a full pump
    // budget is 1.5s and the legacy-CJK encoders sit there), and cost is CLUSTERED by directory. One
    // shard drawing `encoding` would run ~33 minutes while the others idled after two, so the wall
    // clock is set by the unluckiest shard rather than by the work.
    //
    // Round-robin over the SORTED list fixes exactly that, because the sort puts a file's variants and
    // a directory's siblings adjacent -- so the expensive clusters are precisely the ones that get
    // dealt out evenly. Deterministic, no coordination, no shared queue.
    // KORVUS_WPT_FILES=<file>: run exactly these tests, one id per line as the sweep's JSON names them
    // (`path`). The template tier B runs the files its impact map selects this
    // way. Applied BEFORE sharding, so shards deal out the selected set rather than the whole tree.
    if let Ok(p) = std::env::var("KORVUS_WPT_FILES") {
        let want: std::collections::HashSet<String> = std::fs::read_to_string(&p)
            .unwrap_or_else(|e| panic!("KORVUS_WPT_FILES={p}: {e}"))
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        files.retain(|f| want.contains(&f.id()));
        eprintln!("KORVUS_WPT_FILES: {} of {} requested ids found", files.len(), want.len());
    }
    if let Ok(spec) = std::env::var("KORVUS_WPT_SHARD") {
        if let Some((i, n)) = spec.split_once('/') {
            if let (Ok(i), Ok(n)) = (i.trim().parse::<usize>(), n.trim().parse::<usize>()) {
                if n > 0 && i < n {
                    let mut k = 0usize;
                    files.retain(|_| { let keep = k % n == i; k += 1; keep });
                }
            }
        }
    }

    // Optional substring filter for per-file probing (KORVUS_WPT_ONLY=<substr>).
    if let Ok(only) = std::env::var("KORVUS_WPT_ONLY") {
        files.retain(|f| f.id().contains(&only));
    }
    // Optional PRECISE per-interface filter (KORVUS_WPT_FILTER=<Iface>): run only files whose
    // basename interface matches exactly (so DOMRect selects DOMRect-*.html, not DOMRectList-*).
    // The gen tool's `webapi test <iface>` sets this: 34s full run -> ~1s for one interface, and it
    // sidesteps unrelated files (e.g. the dom-abort class) entirely.
    if let Ok(iface) = std::env::var("KORVUS_WPT_FILTER") {
        // Case-insensitive: WPT basenames are inconsistently cased (DOMRect-*.html vs
        // urlsearchparams-*.any.js), so match the interface name ignoring ASCII case.
        //
        // KORVUS_WPT_SECTIONS widens that by PATH. The `html/` tree is laid out by HTML-spec
        // section, so the 177 files exercising `<input>` live under `the-input-element/` and only
        // one carries the interface in its name -- a basename-only filter runs that one file and
        // reports `wpt -`. The gen tool computes the directory names (it owns Blink's tag table)
        // and passes them here, so this side stays a plain path match with no table to keep in sync.
        let sections: Vec<String> = std::env::var("KORVUS_WPT_SECTIONS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        files.retain(|f| {
            basename_iface(&f.rel).eq_ignore_ascii_case(&iface)
                || f.rel.split('/').any(|seg| sections.iter().any(|s| s == seg))
        });
    }
    // Optional cap for fast iteration/diagnosis (KORVUS_WPT_LIMIT=N runs the first N files).
    if let Ok(lim) = std::env::var("KORVUS_WPT_LIMIT") {
        if let Ok(n) = lim.parse::<usize>() {
            files.truncate(n);
        }
    }

    // The second half of the corpus announcement (see the `wpt corpus root:` line at the top of this
    // test): what the walk found, and what this process will actually run once the shard and the
    // KORVUS_WPT_{DIRS,ONLY,FILTER,SECTIONS,LIMIT} filters have had their say. Every selector that
    // narrowed the set is named with its value, so the line reproduces the run.
    let mut selectors: Vec<String> = Vec::new();
    for k in [
        "KORVUS_WPT_DIRS",
        "KORVUS_WPT_SHARD",
        "KORVUS_WPT_FILES",
        "KORVUS_WPT_ONLY",
        "KORVUS_WPT_FILTER",
        "KORVUS_WPT_SECTIONS",
        "KORVUS_WPT_LIMIT",
        "KORVUS_WPT_EXCLUDE",
    ] {
        if let Ok(v) = std::env::var(k) {
            if !v.is_empty() {
                selectors.push(format!("{k}={v}"));
            }
        }
    }
    eprintln!(
        "wpt corpus: {discovered} test URLs discovered, {} selected{}",
        files.len(),
        if selectors.is_empty() { String::from(" (no filters)") } else { format!(" ({})", selectors.join(" ")) }
    );

    // Pass 1: run every file. No assertions here -- the scoreboard + JSON dump must always emit.
    // Per-file progress + timing prints live under --nocapture (the scoreboard prints only at the end).
    let mut rows: Vec<(&TestFile, Outcome)> = Vec::new();
    for (i, f) in files.iter().enumerate() {
        // EVERY FILE STARTS WITH AN EMPTY COOKIE STORE, as it does when re-run alone. The transport's
        // cookie jar and the `document.cookie` read-model are PROCESS-wide, so a shard of eleven
        // hundred files handed each file the cookies of every file before it -- which only stopped
        // mattering while `document.cookie` ignored `Path`. Since then a cookie another
        // file left on a path this one reads made the CSP reporting files fail in the tier and pass
        // alone, run after run. A new `Engine` per file is a fresh page; this makes it a fresh
        // profile. Here and not in `run_file`, which a parallel unit test also calls.
        transport::reset_cookies();
        let _ = webapi::behavior_rt::take_touched(); // the impact map is per file
        let t0 = std::time::Instant::now();
        let mut o = run_file(&server, &root, f).await;
        o.touched = webapi::behavior_rt::take_touched();
        eprintln!(
            "  [{:>2}/{}] {:>6}ms  {:>3}/{:<3}  {}",
            i + 1,
            files.len(),
            t0.elapsed().as_millis(),
            o.passed,
            o.total,
            f.id()
        );
        rows.push((f, o));
    }

    let overall_passed: usize = rows.iter().map(|(_, o)| o.passed).sum();
    let overall_total: usize = rows.iter().map(|(_, o)| o.total).sum();
    let any_harness_loaded = rows.iter().any(|(_, o)| o.harness_loaded);

    // Per-area aggregation.
    let mut areas: Vec<String> = rows.iter().map(|(f, _)| f.area.clone()).collect();
    areas.sort();
    areas.dedup();
    let mut area_rows: Vec<(String, usize, usize, usize)> = Vec::new(); // (area, passed, total, files)
    for a in &areas {
        let mut p = 0;
        let mut t = 0;
        let mut nf = 0;
        for (f, o) in &rows {
            if &f.area == a {
                p += o.passed;
                t += o.total;
                nf += 1;
            }
        }
        area_rows.push((a.clone(), p, t, nf));
    }

    // Scoreboard (stderr; mirrors surface_parity / obstacle_course).
    eprintln!(
        "\n========== WPT conformance scoreboard (server + load_page; REAL testharness.js) ==========\n\
         SUBTESTS  {overall_passed}/{overall_total} pass  ({:.1}%)   across {} files, {} areas   server={}\n",
        pct(overall_passed, overall_total),
        files.len(),
        areas.len(),
        server,
    );

    // A file whose harness did not finish OK ran an unknown fraction of its assertions, so its
    // pass-rate is not a conformance result — it is a coverage hole wearing one. This is loud because
    // it hides perfectly: when a page throws after registering one subtest, the file reports 1/1 and
    // the corpus reports 100%, while the other 89 assertions never ran. Observed 2026-08-26 —
    // url/ went 79/90 across 11 files to 11/11 across the same 11 files, still printing "100.0%".
    let bad: Vec<&(&TestFile, Outcome)> = rows.iter().filter(|(_, o)| o.harness != 0).collect();
    if !bad.is_empty() {
        let lost: usize = bad.iter().filter(|(_, o)| o.total <= 1).count();
        eprintln!(
            "\n!! {}/{} files did NOT finish cleanly (harness ERROR/TIMEOUT); {} of them registered <=1 subtest.\n             !! Their pass-rate is meaningless — the assertions did not run. Treat the % above as UNSOUND.",
            bad.len(),
            files.len(),
            lost,
        );
        for (f, o) in bad.iter().take(10) {
            let why = match o.harness {
                1 => "ERROR",
                2 => "TIMEOUT",
                _ => "did-not-complete",
            };
            eprintln!("     {why:<16} {:>3} subtests  {}", o.total, f.id());
        }
        if bad.len() > 10 {
            eprintln!("     … and {} more", bad.len() - 10);
        }
    }

    // Per-area, worst pass-rate first (the C2 backlog ordering).
    let mut by_rate = area_rows.clone();
    by_rate.sort_by(|a, b| pct(a.1, a.2).partial_cmp(&pct(b.1, b.2)).unwrap().then(b.2.cmp(&a.2)));
    eprintln!("---- by area (worst pass-rate first) ----");
    for (a, p, t, nf) in &by_rate {
        eprintln!("  [{:<16}] {:>4}/{:<4} {:>5.1}%   {} files", a, p, t, pct(*p, *t), nf);
    }

    // Per-file detail.
    eprintln!("\n---- by file ----");
    for (f, o) in &rows {
        let hs = match (o.completed, o.harness) {
            (true, 0) => "OK",
            (true, 1) => "ERROR",
            (true, 2) => "TIMEOUT",
            (true, _) => "?",
            (false, _) => "PARTIAL",
        };
        eprintln!(
            "  {:>3}/{:<3} {:>5.1}%  harness={:<7} loaded={}  {}",
            o.passed,
            o.total,
            pct(o.passed, o.total),
            hs,
            o.harness_loaded,
            f.id()
        );
        for (name, status, msg) in o.failures.iter().take(3) {
            let m: String = msg.chars().take(90).collect();
            eprintln!("        FAIL[{}] {}  {}", status, truncate(name, 64), m);
        }
        if o.failures.len() > 3 {
            eprintln!("        ... +{} more failing subtests", o.failures.len() - 3);
        }
    }

    // TOP failing areas (next C2 targets): areas with the most failing subtests.
    let mut top: Vec<(String, usize, usize)> =
        area_rows.iter().map(|(a, p, t, _)| (a.clone(), t - p, *t)).collect();
    top.sort_by(|a, b| b.1.cmp(&a.1));
    eprintln!("\n---- TOP failing areas (C2 backlog) ----");
    for (a, fails, t) in top.iter().take(5) {
        eprintln!("  {:<16} {} failing / {} subtests", a, fails, t);
    }

    // JSON dump when asked (mirrors obstacle_course's $KORVUS_OBSTACLE_OUT).
    if let Ok(path) = std::env::var("KORVUS_WPT_OUT") {
        let files_json: Vec<serde_json::Value> = rows
            .iter()
            .map(|(f, o)| {
                serde_json::json!({
                    "path": f.id(),
                    "area": f.area,
                    "passed": o.passed,
                    "total": o.total,
                    "harness_status": o.harness,
                    "errors": o.errors,
                    "completed": o.completed,
                    "harness_loaded": o.harness_loaded,
                    // OBSERVATION ONLY (see Outcome's field doc comments): never fed back into passed/
                    // total/completed above, so a sweep scores identically with these on or off.
                    "tests_registered": o.tests_registered,
                    "hit_pump_budget": o.hit_pump_budget,
                    "touched": o.touched,
                    "hit_watchdog": o.hit_watchdog,
                    "watchdog_terminations": o.watchdog_terminations,
                    "failures": o.failures.iter().map(|(n, s, m)| serde_json::json!({ "name": n, "status": s, "message": m })).collect::<Vec<_>>(),
                    // Every subtest, so the grader can attribute assertions to members within a file.
                    "subtests": o.subtests.iter().map(|(n, s)| serde_json::json!({ "name": n, "status": s })).collect::<Vec<_>>(),
                })
            })
            .collect();
        let areas_json: Vec<serde_json::Value> = area_rows
            .iter()
            .map(|(a, p, t, nf)| serde_json::json!({ "area": a, "passed": p, "total": t, "files": nf }))
            .collect();
        let out = serde_json::json!({
            "harness": "server + load_page; REAL testharness.js",
            "overall_passed": overall_passed,
            "overall_total": overall_total,
            "files_count": files.len(),
            "areas": areas_json,
            "files": files_json,
        });
        std::fs::write(&path, serde_json::to_string_pretty(&out).unwrap())
            .unwrap_or_else(|e| panic!("KORVUS_WPT_OUT write to {path} failed: {e}"));
        eprintln!("\nwrote {path}");
    }

    // Webapi behavior coverage: writes per-member calls/stub_hits to $KORVUS_COVERAGE_OUT
    // if set (no-op otherwise). Pairs with `webapi coverage --dynamic` to show
    // which members this WPT run touched and which of those are still stubs.
    webapi::behavior_rt::dump_coverage_if_requested();

    // Pass 2: floor assertion only. This is a scoreboard, not a gate -- individual WPT subtest failures
    // are the signal (the C2 backlog) and MUST NOT be fatal. We assert only that the harness itself
    // works: real testharness.js loaded over transport, and at least one real WPT subtest ran + passed.
    assert!(
        !files.is_empty(),
        "no WPT test files discovered under {} -- vendoring/discovery is broken",
        root.display()
    );
    assert!(
        any_harness_loaded,
        "real testharness.js did not load in Korvus via the static server (the `test` global was absent \
         for every file) -- the server + load_page transport path is broken"
    );
    assert!(
        overall_total > 0 && overall_passed > 0,
        "WPT harness produced no passing subtests ({overall_passed}/{overall_total}) -- the runner is \
         broken (this floor guards the harness, not individual WPT conformance)"
    );
}

fn pct(n: usize, d: usize) -> f64 {
    if d == 0 {
        0.0
    } else {
        100.0 * n as f64 / d as f64
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let t: String = s.chars().take(n).collect();
        format!("{t}...")
    }
}
