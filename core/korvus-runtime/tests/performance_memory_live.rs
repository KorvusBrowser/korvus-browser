//! `performance.memory` is THIS engine's heap, put through the browser's rule.
//!
//! # What was wrong
//!
//! The three numbers were the persona's, read out of `values.json` and handed back unchanged. For
//! `brave-151-linux-20260907T1006` that is `29754100 / 17100312 / 4395630592` — on every read, in
//! every document, forever. A page that allocates 300 MB and reads again sees the same
//! `usedJSHeapSize`; so does a page that reads twice across a navigation. No Chromium behaves that
//! way in either of its two modes, and the frozen-ness is a much louder tell than the magnitude the
//! defect was first reported as: it takes one allocation and two reads to see.
//!
//! # The rule these tests assert
//!
//! Measured 2026-09-18 against **all four installed browsers** — Brave 151.1.93.138
//! (Chrome/151.0.7922.173), Chrome/150.0.7871.46, Edg/151.0.4129.93 and Opera (Chrome/151.0.7922.176),
//! each launched on a DevTools port chosen by the harness (never `=0`, L18.1) against a loopback
//! fixture. All four answer identically; there is no vendor row here.
//!
//!   * a document with a TUPLE ORIGIN (the renderer is locked to a site) reports the raw V8 heap;
//!   * `about:blank`, `data:` and `file:` — no tuple origin — report a value QUANTIZED onto a ladder
//!     of 100 buckets, the smallest bucket that is not smaller than the size, clamped to the last;
//!   * a reading is LATCHED: 50 ms on the precise path, 20 minutes on the quantized one.
//!
//! [`ladder`] below rebuilds that ladder from the measured description, independently of the engine,
//! so these assertions are about the RULE and never about a byte count this engine happens to print.
//! The sixteen distinct values actually read out of the four browsers are in
//! [`the_ladder_contains_every_value_the_four_browsers_printed`], and they are the only literals in
//! this file that came off a browser.

use engine::Engine;

/// A document with a tuple origin: the renderer is locked to a site, so Chromium answers precisely.
/// Nothing here touches the network — `load_page` only carries the URL.
const LOCKED: &str = "http://127.0.0.1:8731/heap.html";
/// A document with NO tuple origin, the case all four browsers quantize.
const UNLOCKED: &str = "about:blank";

const HTML: &str = r#"<!doctype html><html><body>heap</body></html>"#;

/// Chromium's bucket ladder, rebuilt from the measured description rather than from the engine:
/// 100 buckets from 10 MB to 4 GB, each rounded DOWN to three significant decimal digits.
fn ladder() -> Vec<f64> {
    let ratio = (4_000_000_000.0f64 / 10_000_000.0).powf(1.0 / 100.0);
    (0..100)
        .map(|i| {
            let v = 10_000_000.0 * ratio.powi(i);
            let step = 10f64.powi(v.log10().floor() as i32 - 2);
            (v / step).floor() * step
        })
        .collect()
}

async fn probe(url: &str, js: &str) -> Vec<String> {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_page(HTML, url, &[]).await.expect("load_page");
    e.eval("t", js.into()).unwrap();
    e.run().await.unwrap();
    e.take_output()
}

/// Read the three numbers once, as `total,used,limit`.
const READ: &str = r#"
    const m = performance.memory;
    print([m.totalJSHeapSize, m.usedJSHeapSize, m.jsHeapSizeLimit].join(","));
"#;

fn triple(out: &[String]) -> [f64; 3] {
    let row = out.last().expect("a reading");
    let n: Vec<f64> = row.split(',').map(|s| s.parse().expect("a number")).collect();
    [n[0], n[1], n[2]]
}

/// THE SIXTEEN VALUES the four browsers printed, and the only browser literals in this file. Every
/// one of them is a member of the ladder — which is what makes [`ladder`] the browsers' ladder and
/// not merely an internally consistent one.
#[test]
fn the_ladder_contains_every_value_the_four_browsers_printed() {
    let l = ladder();
    for v in [
        10_000_000.0,
        13_400_000.0,
        14_300_000.0,
        21_700_000.0,
        23_100_000.0,
        33_100_000.0,
        50_400_000.0,
        64_000_000.0,
        97_400_000.0,
        139_000_000.0,
        212_000_000.0,
        322_000_000.0,
        553_000_000.0,
        842_000_000.0,
        1_280_000_000.0,
        3_760_000_000.0,
    ] {
        assert!(l.contains(&v), "{v} is not on the ladder: {l:?}");
    }
    // The ladder is strictly increasing, and its granularity is the point: consecutive buckets are
    // at least 5 % apart, so a quantized reading can never resolve a small allocation.
    for w in l.windows(2) {
        assert!(w[1] > w[0], "not increasing at {w:?}");
        assert!(w[1] / w[0] > 1.05, "too fine at {w:?}");
    }
    assert_eq!(l.len(), 100);
    assert_eq!(l[0], 10_000_000.0);
}

/// A document with no tuple origin reports ladder values, and the limit clamps to the ladder's top.
///
/// `jsHeapSizeLimit` is the one that proves the clamp: the persona's limit (4395630592) is ABOVE the
/// last bucket, so every quantized page in every one of the four browsers reports the last bucket
/// for it. This asserts `l.last()`, never the number.
#[tokio::test]
async fn an_unlocked_document_reports_only_ladder_values() {
    let l = ladder();
    let [total, used, limit] = triple(&probe(UNLOCKED, READ).await);
    assert!(l.contains(&total), "totalJSHeapSize {total} is off the ladder");
    assert!(l.contains(&used), "usedJSHeapSize {used} is off the ladder");
    assert_eq!(limit, *l.last().unwrap(), "the limit clamps to the top of the ladder");
    assert!(used >= l[0] && total >= l[0], "a quantized reading is never below the first bucket");
    assert!(used <= total, "used <= total");
}

/// GROWTH UNDER ALLOCATION, on the quantized path: a page that allocates before its FIRST read
/// reports a higher bucket than an idle one, and one at least as large as what it allocated.
///
/// It has to be the first read because of the latch — see
/// [`a_quantized_reading_is_latched_against_a_page_that_polls`], which is the same fact from the
/// other side.
#[tokio::test]
async fn an_unlocked_document_grows_by_whole_buckets() {
    let idle = triple(&probe(UNLOCKED, READ).await);
    let grown = triple(
        &probe(
            UNLOCKED,
            r#"
        globalThis.__k = [];
        for (let i = 0; i < 200; i++) __k.push(new Uint8Array(1024 * 1024).fill(i));
        const m = performance.memory;
        print([m.totalJSHeapSize, m.usedJSHeapSize, m.jsHeapSizeLimit].join(","));
    "#,
        )
        .await,
    );
    assert!(ladder().contains(&grown[1]), "grown usedJSHeapSize {} is off the ladder", grown[1]);
    assert!(grown[1] > idle[1], "200 MB did not move the bucket: {} -> {}", idle[1], grown[1]);
    assert!(
        grown[1] >= 200.0 * 1024.0 * 1024.0,
        "the bucket must cover what was allocated, got {}",
        grown[1]
    );
    assert_eq!(grown[2], idle[2], "the limit does not depend on the heap");
}

/// THE LATCH, quantized side: a page that has already read cannot watch its own allocation land.
/// Measured in all four browsers — first read, then 300 MB, still the first bucket 90 s later.
#[tokio::test]
async fn a_quantized_reading_is_latched_against_a_page_that_polls() {
    let out = probe(
        UNLOCKED,
        r#"
        const before = performance.memory.usedJSHeapSize;
        globalThis.__k = [];
        for (let i = 0; i < 200; i++) __k.push(new Uint8Array(1024 * 1024).fill(i));
        const t = performance.now();
        while (performance.now() - t < 200) { /* well past the precise path's 50 ms */ }
        print(before + "," + performance.memory.usedJSHeapSize);
    "#,
    )
    .await;
    let row = out.last().unwrap();
    let (a, b) = row.split_once(',').unwrap();
    assert_eq!(a, b, "the quantized reading moved inside its 20-minute window: {row}");
}

/// A document WITH a tuple origin reports the raw heap: `used <= total`, and a resolution the
/// quantizer does not have.
///
/// "Not quantized" is asserted as the ladder's own predicate, not as a magnitude: a bucket is a
/// small integer times a power of ten (three significant digits), so a raw byte count that carries a
/// fourth significant digit cannot be one. That is a statement about the RULE and it stays true
/// however large this engine's heap grows.
#[tokio::test]
async fn a_locked_document_reports_the_raw_heap() {
    let l = ladder();
    let [total, used, limit] = triple(&probe(LOCKED, READ).await);
    assert!(used > 0.0 && total >= used, "used {used} total {total}");
    for (name, v) in [("totalJSHeapSize", total), ("usedJSHeapSize", used)] {
        assert!(!l.contains(&v), "{name} {v} is a bucket, so it was quantized on a locked document");
        let step = 10f64.powi(v.log10().floor() as i32 - 2);
        assert!(
            (v / step).fract() != 0.0,
            "{name} {v} has only three significant digits — it cannot be told from a bucket"
        );
    }
    // `jsHeapSizeLimit` is the persona's, unquantized, on a locked document. It is the one row that
    // already matched the reference browser and this unit does not move it.
    assert_eq!(limit, 4_395_630_592.0);
}

/// The precise path's latch, in ms. It is the browsers' number and not this engine's: measured at
/// 1 ms resolution on all four (the measurement block in `webapi/src/core/timing/performance.rs`),
/// 50 ms apart still reports the old reading and 52 ms apart reports the new one.
const LATCH_MS: f64 = 50.0;

/// GROWTH UNDER ALLOCATION, precise side, and THE LATCH from the other direction in one page: two
/// reads 64 MiB apart but inside 50 ms are EQUAL, a read past 50 ms has refreshed, and the reading
/// covers every byte the page is holding live.
///
/// # PM1: what this test asserted until 2026-09-21, and why the reference browser fails it
///
/// It asserted `after - before >= 64 MiB` with zero headroom, and it measured the gap between its
/// two reads as if the only thing between them were its own `wait(10)`. Both were wrong, and the
/// second is what actually reddened the gate — three units (TLA1, DA1, ST1) measured it on the
/// unmodified tree at 2/10, 10/40 and 0/10 while each was dispatched elsewhere.
///
/// **THE GAP.** `new Uint8Array(64 MiB).fill(7)` sat between the two reads, and filling 64 MiB is
/// not free: measured here, 15–120 ms depending on the box's load (median 23 ms at loadavg 20,
/// 57 ms at loadavg 35). The `wait(10)` was therefore never the gap; the gap was `fill` plus 10 ms,
/// and once it crossed 50 ms the engine refreshed the reading — **correctly** — and `assert_eq!`
/// failed. Over 60 reps of the old test: 1/30 failures at loadavg 17–20 and **22/30 at loadavg
/// 28–36, every one of them this assertion**. It was detecting the box, and calling correct engine
/// behaviour a defect. Brave, asked the same question with the same page, holds the latch 10/10
/// across a 0.1 ms allocation and refreshes 10/10 at a gap of 80 ms — and registers the whole
/// backing store of `new Uint8Array(n)` **without** `.fill()`, in 0.0–0.7 ms. The fill bought
/// nothing this test reads and cost the window this test depends on, so it is gone; the gap is now
/// MEASURED on the page's own clock and the rule asserted against it rather than assumed.
///
/// **THE GROWTH.** `after - before >= allocated` is not a property of the platform. Asked directly —
/// Brave 151, loopback tuple origin, precise path, 14 reps of exactly this page — it came out SHORT
/// in **6 of 14**, three of them at roughly **minus 67 MB**, because a collection between the two
/// reads freed what the previous document had been holding. A difference of two heap samples has no
/// lower bound a collector must respect, so no engine can be held to one. What all 14 reps DID hold,
/// over 14 post-latch reads and 336 polls, is the bound a collector cannot cross: **the reading is
/// never below what the page is holding live**. That is what is asserted here, with
/// `before < allocated` beside it so the pair still says the number MOVED — the frozen persona
/// reading this whole file exists for is 17100312, which fails that on sight.
///
/// The 64 MiB is rooted at `globalThis.__k`, so no collector may take it: verified by the live-bytes
/// bound holding in 60/60 reps of this engine and 14/14 of Brave.
#[tokio::test]
async fn a_locked_document_grows_by_what_the_page_allocated() {
    let out = probe(
        LOCKED,
        r#"
        const wait = (ms) => { const t = performance.now(); while (performance.now() - t < ms) {} };
        // t0/t1 bracket the read that SETS the latch and t2/t3 the read that tests it, so the true
        // gap is known to lie in [t2 - t1, t3 - t0] and neither branch below is ever asserted on
        // the wrong side of the window.
        const t0 = performance.now();
        const before = performance.memory.usedJSHeapSize;
        const t1 = performance.now();
        // NO `.fill()`: measured in Brave, the backing store counts the moment it is allocated, and
        // filling it costs 15-120 ms out of the 50 ms window the next read has to land inside.
        globalThis.__k = [new Uint8Array(64 * 1024 * 1024)];
        wait(10);
        const t2 = performance.now();
        const inside = performance.memory.usedJSHeapSize;
        const t3 = performance.now();
        wait(120);
        const after = performance.memory.usedJSHeapSize;   // past the window, whatever the load
        print([before, inside, after, t2 - t1, t3 - t0].join(","));
    "#,
    )
    .await;
    let n: Vec<f64> = out.last().unwrap().split(',').map(|s| s.parse().unwrap()).collect();
    let (before, inside, after, gap_lo, gap_hi) = (n[0], n[1], n[2], n[3], n[4]);
    let allocated = 64.0 * 1024.0 * 1024.0;

    // THE LATCH, as the rule rather than as an assumption about how long an allocation takes.
    // Exactly one of these is asserted on any run; the sub-millisecond band in which the page's own
    // clock cannot say which side of the window the read fell on is the one case neither covers.
    if gap_hi < LATCH_MS {
        let w = LATCH_MS;
        assert_eq!(before, inside, "a precise reading moved inside its {w} ms window (gap <= {gap_hi} ms)");
    } else if gap_lo >= LATCH_MS {
        assert_ne!(before, inside, "a precise reading did not refresh {gap_lo} ms after the last one");
    }

    // THE ALLOCATION IS ACCOUNTED FOR. `before < allocated` is the measurement's precondition and is
    // what makes the pair say the reading MOVED: an idle context here reads about 9.99 MB, and no
    // frozen number below 64 MiB — the persona's 17100312 among them — can satisfy the second line.
    assert!(
        before < allocated,
        "the baseline heap {before} is not smaller than the {allocated} the page is about to allocate"
    );
    assert!(
        after >= allocated,
        "the page holds {allocated} live and usedJSHeapSize reports {after} ({before} -> {after})"
    );
    assert!(
        after - before < allocated * 1.5,
        "the growth is the allocation, not an invented number: {} for {allocated}",
        after - before
    );
}

/// The heap is THE ENGINE'S, so two documents of the same persona that allocated different amounts
/// disagree — which is precisely what the frozen persona number could not do.
#[tokio::test]
async fn two_documents_of_one_persona_do_not_report_the_same_heap() {
    let small = triple(&probe(LOCKED, READ).await);
    let big = triple(
        &probe(
            LOCKED,
            r#"
        globalThis.__k = [];
        for (let i = 0; i < 32; i++) __k.push(new Uint8Array(1024 * 1024).fill(i));
        const m = performance.memory;
        print([m.totalJSHeapSize, m.usedJSHeapSize, m.jsHeapSizeLimit].join(","));
    "#,
        )
        .await,
    );
    assert!(big[1] > small[1], "the heap did not follow the page: {} vs {}", small[1], big[1]);
    assert_eq!(big[2], small[2], "the limit is the persona's in both");
}
