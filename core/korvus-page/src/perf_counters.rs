//! Process-wide work counters for the style, layout and animation passes.
//!
//! They count WORK, not time: how many elements a pass styled, how many boxes it built and laid out,
//! how many elements an animation rescan visited. They are what the incremental passes are
//! measured by, next to instructions. A count is a relaxed atomic add, and nothing reads
//! them on any page path.
//!
//! `KORVUS_PERF_COUNTERS=<file>` makes [`flush`] write the running totals to that file, as one JSON
//! object, at the end of every layout and every animation rescan. The last write holds the totals.
//!
//! They count only in a diag build (the `perf-counters` feature). Without it [`add`] and [`flush`]
//! are empty and the counters stay 0, so a release binary pays nothing for them: www.mapbox.com's load
//! adds to them 33 M times.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::OnceLock;

macro_rules! counters {
    ($($name:ident),* $(,)?) => {
        $(pub static $name: AtomicU64 = AtomicU64::new(0);)*

        fn snapshot() -> String {
            let mut out = String::from("{");
            $(
                if out.len() > 1 {
                    out.push(',');
                }
                out.push_str(&format!("\"{}\":{}", stringify!($name).to_ascii_lowercase(), $name.load(Relaxed)));
            )*
            out.push('}');
            out
        }
    };
}

counters!(
    LAYOUT_DOCUMENTS,
    LAYOUT_PASSES,
    BOXES_CARRIED,
    ELEMENTS_STYLED,
    ELEMENTS_MATCHED,
    BOXES_BUILT,
    TAFFY_ROOT_RUNS,
    BOX_LAYOUTS_COMPUTED,
    EXTRA_PASSES_PADDING,
    EXTRA_PASSES_CV,
    EXTRA_PASSES_SCROLLBAR,
    DECLARATIVE_UPDATES,
    DECLARATIVE_RESCANS,
    DECLARATIVE_ELEMENTS_VISITED,
    DECLARATIVE_CASCADES,
    COMPUTED_STYLE_CALLS,
    VERIFY_PASSES,
    VERIFY_MISMATCHES,
    STYLE_PASSES_FULL,
    STYLE_PASSES_INCREMENTAL,
    FALLBACK_BASIS,
    FALLBACK_OVERFLOW,
    FALLBACK_CONTAINER,
    FALLBACK_CQ_UNITS,
    FALLBACK_SCOPE,
    FALLBACK_SHADOW,
    FALLBACK_ROOT_FONT,
    FALLBACK_OTHER,
    MARKS_ATTRIBUTE,
    MARKS_REACH_SUBTREE,
    MARKS_REACH_SIBLINGS,
    MARKS_REACH_KEYED,
    MARKS_INSERTED,
    MARKS_CHILDREN_ALL,
    MARKS_CHILDREN_LATER,
    MARKS_CHILDREN_EDGE,
    MARKS_EMPTY,
    MARKS_HAS,
    MARKS_HOVER,
    MARKS_FORM,
    MARKS_ANIMATED,
    ELEMENTS_RESTYLED_BY_SUBTREE,
    LAYOUT_TREES_IDENTICAL,
    LAYOUT_ELEMENT_BOXES,
    LAYOUT_ELEMENT_BOXES_CLEAN,
    LAYOUTS_KEPT,
    DECLARATIVE_RESCANS_INCREMENTAL,
    DECLARATIVE_STARTED_OVER_FIRST,
    DECLARATIVE_STARTED_OVER_CONTAINER,
    DECLARATIVE_STARTED_OVER_SHADOW,
    DECLARATIVE_STARTED_OVER_SCOPE,
    DECLARATIVE_STARTED_OVER_ROOT,
    DECLARATIVE_TOUCHED,
    DECLARATIVE_CHAINED,
    DECLARATIVE_SETS_REWORKED,
    MARKS_BELOW_CONTAINERS,
    MARKS_SHEETS,
    SHEET_CHANGES_PLANNED,
    PSEUDO_RESTYLES_ALL,
    COMPUTED_STYLES_KEPT,
    MEASURES_REFUSED_MARGIN,
    FLEX_FLIP_ITEM_CLEARS,
    MARGIN_CACHE_HITS,
    CANONICAL_HITS,
    SLOT_STATES_REUSED,
    LAYOUT_GUTTED_FALLBACK,
);

/// Add `n` to `counter`.
#[inline(always)]
pub fn add(counter: &AtomicU64, n: u64) {
    if cfg!(feature = "perf-counters") {
        counter.fetch_add(n, Relaxed);
    }
}

fn out_path() -> Option<&'static str> {
    static OUT: OnceLock<Option<String>> = OnceLock::new();
    OUT.get_or_init(|| std::env::var("KORVUS_PERF_COUNTERS").ok()).as_deref()
}

/// Write the running totals to `$KORVUS_PERF_COUNTERS`, when it is set.
pub fn flush() {
    if !cfg!(feature = "perf-counters") {
        return;
    }
    if let Some(path) = out_path() {
        let _ = std::fs::write(path, snapshot());
    }
}
