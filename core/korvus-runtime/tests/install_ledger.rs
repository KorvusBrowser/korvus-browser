//! THE INSTALL LEDGER GATE — the enforcement of one install per member.
//!
//! # Why the test I2 originally specified could not work
//!
//! I2 promised a test that "asserts every own string-keyed member of every captured interface
//! prototype/object is in the ledger and nothing in the ledger is missing". Measured at master
//! `80398126` (lane L20), the file did not exist — and had it existed it would have passed over the
//! defect it was written for. Three reasons, each structural:
//!
//! * **Two writers on one member produce ONE own member and ONE ledger row.** An every-member-is-
//!   present check cannot see a second writer at all. `DOMRect`/`DOMPoint` carried an `interface!`
//!   table AND a JS `num()` block redefining their setters for as long as anyone could date it.
//! * **The ledger records what the INSTALLER does.** A JS `Object.defineProperty` makes no ledger
//!   call, so a second writer that is not the installer leaves no trace in it whatsoever.
//! * **Its only consumer, `installer_parity::assert_ledger`, walks the GENERATED registries in a bare
//!   `JsRuntime` with no ESM layer.** Hand tables — now the majority — were never in its scope, and
//!   neither was any JS that runs after the registry install.
//!
//! # What this asks instead
//!
//! Not "is the row in the ledger" but **"is what the page SEES in that slot still what the installer
//! put there"**, over a real `Engine` with the whole ESM layer and both realms.
//!
//! Two mechanisms in `install/installer.rs` answer it:
//!
//! * The ledger **REJECTS** a second `(realm, interface, member, holder)` record. The first writer
//!   keeps the row and the collision is published naming both passes. That is installer-vs-installer,
//!   and [`the_ledger_rejects_a_second_record`] holds it at zero.
//! * Every function the one member mount leaves in a slot is branded with the row that mounted it
//!   (`installer::stamp_member`), and the descriptor flags it ended up with are recorded beside it.
//!   `installer::audit_surface` reads both back off the live surface at the end of the last install
//!   pass in each realm. A slot holding an unbranded function was taken over by a writer that is not
//!   the installer; a slot holding the installer's own function under different flags was
//!   re-described by one.
//!
//! # The part that decides whether this survives
//!
//! Some second writes are deliberate and correct. The re-aim pass (deleted with the
//! last member copy) re-aimed a member's receiver check AFTER it was mounted, and W4.f retired three install-order contracts that
//! existed precisely so a JS decorator could wrap a table's row. A check that scored those as
//! clobbers would be switched off within a week.
//!
//! **Nothing in the ledger row as it stood could tell them apart.** `(interface, member, holder,
//! pass)` says what the installer did and nothing about the object in the slot, so a re-aim and a
//! clobber are byte-identical to it. The brand is the column that was missing, and the distinction is
//! DECLARED rather than guessed: the steps that legitimately rebuild a member's function — the arity
//! guard, and `receiver::rebind` until it was deleted — carry the brand across the wrapper they build, so they
//! report as `ok` or `reaim:<row>`, while a writer that does not declare itself reports as
//! `clobber:*`. That is the honest shape of the distinction: it is not derivable from the descriptor.
//!
//! # Why this file is its own test binary
//!
//! Declared in `Cargo.toml`, not in `tests/all.rs` — the second file in this crate that has to be
//! (see that file's header for the first). The install ledger is PROCESS-GLOBAL, because a worker
//! realm is assembled on another thread and its verdicts have to reach this gate somehow. Inside the
//! `all` binary every other test's `Engine` writes into the same ledger, 750 of them in eight
//! threads, in configurations this gate never asked for. Measured on this tree: **41 second writers
//! run alone, 36 run inside `all`**, with three rows appearing and eight disappearing between the
//! two. A gate whose number moves with its neighbours is not a gate. Within its own process the
//! merge is order-independent too — the WEAKEST verdict for a row wins, the same rule
//! `behavior_rt::record_layer` uses and for the same reason: a member a second writer owns in any
//! realm is owned by one as far as a page is concerned, and a ledger that flatters is the failure
//! mode W7 spent a whole track undoing.
//!
//! # This gate READS the surface; it does not write it
//!
//! `installer::audit_surface` runs as the last statement of `finalize_surface`, which is a pass added
//! to every page load, so the question "did adding it change what a page sees" has to be answered
//! rather than assumed. It was, by measuring `scripts/coverage.sh` at the base commit `88281726` and
//! on this tree: **5618 verified / 5323 independent / 7298 present, cases 13322/14909, on both** —
//! byte-identical, every tell counter 0. The brand is a `v8::Private`, so it is invisible to
//! `Object.getOwnPropertyNames`, `Object.getOwnPropertySymbols` and `Reflect.ownKeys` alike;
//! confirmed on the rebuilt release binary, where `DOMRect.prototype` still enumerates
//! `x,y,width,height,constructor`, its `x` getter owns no symbols, and `Element.getAttribute` owns
//! exactly `length,name`.
//!
//! # The remainder, and why it is a manifest rather than a number
//!
//! The clobbers are real and this unit does not delete them: they are `op_helper_*` bodies in
//! `korvus-runtime/src/lib.rs` string literals and `defineProperty` blocks in the shipped JS, which
//! W4/W5 remove with the layers they live in. So the pinned truth is a SORTED MANIFEST of them
//! (invariant I5's shape, not a magic number): a NEW second writer fails this gate by name, a removed
//! one fails it asking to be recorded. `KORVUS_UPDATE_SECOND_WRITERS=1` rewrites it.

use engine::Engine;
use std::collections::BTreeMap;

fn map_of(json: String) -> BTreeMap<String, String> {
    serde_json::from_str::<serde_json::Value>(&json)
        .unwrap()
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
        .collect()
}

fn manifest_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/second_writers.manifest")
}

/// A window realm with the whole ESM layer, a document and a navigation — i.e. every pass that runs
/// on a real page, which is the thing `installer_parity`'s bare `JsRuntime` cannot reach.
async fn window_realm() -> Engine {
    // The audit runs only when asked: this gate is its reader.
    webapi::install::installer::enable_install_audit();
    // THE BUNDLE THIS RUN WEARS: `$KORVUS_BUNDLE` if set, else the embedded one -- not `default()`,
    // the embedded bundle whatever the environment says. Presence and order follow the ACTIVE bundle,
    // so `KORVUS_BUNDLE=<id>` has to reach this gate or it can only ever grade one device.
    let mut e = Engine::new(Box::new(
        persona::CapturedPersona::select(std::env::var("KORVUS_BUNDLE").ok().as_deref()).unwrap_or_else(|e| panic!("{e}")),
    ));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.run().await.unwrap();
    e
}

/// Drive a REAL DOM worker to completion, so the worker realm's install runs and its audit with it.
/// The worker realm is assembled on another thread; the ledgers it fills are process-global, which is
/// what lets one test see both realms — and, for the same reason, why this file is its own binary.
async fn worker_realm() {
    let mut e = window_realm().await;
    let reg = format!(
        "globalThis.__workerScripts = {{ './probe.js': {} }};",
        serde_json::to_string("postMessage(typeof navigator);").unwrap()
    );
    e.eval("worker-scripts", reg).unwrap();
    e.run().await.unwrap();
    e.eval("driver", "const w = new Worker('./probe.js'); w.onmessage = (ev) => print('RESULT:' + ev.data);".into()).unwrap();
    let start = std::time::Instant::now();
    let mut out = Vec::new();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if out.iter().any(|l| l.starts_with("RESULT:")) {
            break;
        }
        if start.elapsed() > std::time::Duration::from_secs(20) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(
        out.iter().any(|l| l.starts_with("RESULT:")),
        "the worker never answered, so the worker realm was never installed and this gate would be \
         measuring one realm while claiming two: {out:?}"
    );
}

/// INSTALLER-VS-INSTALLER. Two of this installer's own passes mounting the same slot is the failure
/// the ledger itself can see, and it must be empty — if two passes want one slot, one of them is
/// wrong, and "skip if already owned" is the convention this whole goal deletes.
#[tokio::test]
async fn the_ledger_rejects_a_second_record() {
    worker_realm().await;
    let dups = map_of(webapi::install::installer::install_duplicates_json());
    for (k, v) in &dups {
        eprintln!("  REJECTED second record: {k}  ({v})");
    }
    assert!(
        dups.is_empty(),
        "{} slot(s) were recorded twice by the installer itself. The ledger kept the FIRST writer and \
         rejected the second; the second writer's pass is named after the `+`. Delete one of the two \
         rows -- a member is installed exactly once, by exactly one row:\n{}",
        dups.len(),
        dups.iter().map(|(k, v)| format!("  {k}  {v}")).collect::<Vec<_>>().join("\n")
    );
}

/// THE CLASSIFIED REMAINDER. Every slot the ledger claims, in both realms, with what the page sees in
/// it now. `ok` and `reaim:` are the installer's own work; `clobber:` and `flags:` are second writers
/// and are pinned by name.
#[tokio::test]
async fn every_slot_the_ledger_claims_still_holds_what_the_installer_put_there() {
    worker_realm().await;
    let audit = map_of(webapi::install::installer::install_audit_json());
    assert!(
        audit.len() > 3000,
        "only {} rows were audited -- the ledger is not being read back, and an empty gate looks \
         exactly like a passing one",
        audit.len()
    );

    let mut ok = 0usize;
    let mut reaim: Vec<String> = Vec::new();
    let mut second: Vec<String> = Vec::new();
    let mut by_class: BTreeMap<&str, usize> = BTreeMap::new();
    for (k, v) in &audit {
        let class = v.split(':').next().unwrap_or("");
        *by_class.entry(match class {
            "ok" => "ok",
            "reaim" => "reaim",
            "flags" => "flags",
            "gone" => "gone",
            _ => "clobber",
        })
        .or_default() += 1;
        match class {
            "ok" => ok += 1,
            "reaim" => reaim.push(format!("{k}\t{v}")),
            _ => second.push(format!("{k}\t{v}")),
        }
    }
    let realms: std::collections::BTreeSet<&str> = audit.keys().filter_map(|k| k.split('|').next()).collect();
    eprintln!("install ledger audit: {} rows, {ok} still the installer's own", audit.len());
    for (c, n) in &by_class {
        eprintln!("  {c}: {n}");
    }
    eprintln!("  realms audited: {realms:?}");
    // By INTERFACE, because a second writer is a file redefining a family and the tally is what names
    // the file to delete.
    let mut by_iface: BTreeMap<String, usize> = BTreeMap::new();
    for l in &second {
        let row = l.split('\t').next().unwrap_or(l);
        let iface = row.split_once('|').map(|p| p.1).unwrap_or(row).split("::").next().unwrap_or(row);
        *by_iface.entry(iface.to_string()).or_default() += 1;
    }
    let mut tally: Vec<(&String, &usize)> = by_iface.iter().collect();
    tally.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    for (iface, n) in tally.iter().take(40) {
        eprintln!("    second writer x{n}: {iface}");
    }

    assert!(
        realms.contains("window") && realms.contains("worker"),
        "both realms must be audited, saw {realms:?} -- a gate that sees one realm and reports on two \
         is the hole `installer_parity` already had"
    );

    second.sort();
    let body = format!(
        "# GENERATED by `KORVUS_UPDATE_SECOND_WRITERS=1 cargo test -p korvus-runtime --test \
         install_ledger` -- every slot the install ledger claims that a SECOND WRITER owns now.\n\
         # `clobber:engine` a `build_fn` body the installer did not mount (op_helper_*) \
         · `clobber:unbranded` a function with no engine brand at all (glue JS, or a raw \
         v8::Function::builder) · `flags:` the installer's own function, re-described by someone else \
         · `gone` the slot the installer mounted no longer exists.\n\
         # This only ever SHRINKS (invariant I2 ends at zero). A new entry is a new second writer.\n{}\n",
        second.join("\n")
    );
    if std::env::var("KORVUS_UPDATE_SECOND_WRITERS").is_ok() {
        std::fs::create_dir_all(manifest_path().parent().unwrap()).unwrap();
        std::fs::write(manifest_path(), &body).unwrap();
        eprintln!("wrote {}", manifest_path().display());
        return;
    }
    let want = std::fs::read_to_string(manifest_path()).unwrap_or_default();
    if want == body {
        return;
    }
    let want_rows: std::collections::BTreeSet<&str> = want.lines().filter(|l| !l.starts_with('#') && !l.is_empty()).collect();
    let now_rows: std::collections::BTreeSet<&str> = second.iter().map(String::as_str).collect();
    let added: Vec<&&str> = now_rows.difference(&want_rows).collect();
    let removed: Vec<&&str> = want_rows.difference(&now_rows).collect();
    assert!(
        added.is_empty(),
        "{} NEW second writer(s) own a slot the install ledger claims. A member is installed exactly \
         once, by exactly one row -- put the body in the interface's table instead of redefining the \
         slot after the installer mounted it:\n{}",
        added.len(),
        added.iter().map(|s| format!("  {s}")).collect::<Vec<_>>().join("\n")
    );
    assert!(
        removed.is_empty(),
        "{} second writer(s) are GONE (good) -- record it: \
         KORVUS_UPDATE_SECOND_WRITERS=1 cargo test -p korvus-runtime --test install_ledger \
         and commit the manifest with this change:\n{}",
        removed.len(),
        removed.iter().map(|s| format!("  {s}")).collect::<Vec<_>>().join("\n")
    );
}
