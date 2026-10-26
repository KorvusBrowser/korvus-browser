//! Own-global ORDER fidelity: how closely `Object.getOwnPropertyNames(globalThis)` in a Korvus realm
//! reproduces the order the capture bundle recorded for the same realm.
//!
//! WHY THIS EXISTS. Property order is observable. `Object.getOwnPropertyNames(globalThis)` returns a
//! DEFINED sequence -- V8 hands back integer-like keys ascending, then string keys in creation order --
//! and a fingerprinter can read the whole list in one call. Every other global-scope check in this tree
//! compares SETS: `quality::scan_globals` reports `extra_globals` / `missing_globals`, and
//! `tests/worker_surface.rs` asserts `GLOBAL-TELLS 0` and `MISSING-GLOB 0`. All of them are satisfied by
//! a realm that exposes exactly the right names in exactly the wrong sequence, which is the realm this
//! engine had when this module was written: the worker's sets matched the bundle's perfectly and 274 of
//! its 335 positions still disagreed. Two units measured that in passing with throwaway probes and
//! reverted them, so the number was folklore. This module is the instrument that stops it being folklore.
//!
//! THE TWO METRICS, AND WHY THE GATE RATCHETS ON ONLY ONE OF THEM.
//!
//! * `positions_agree` -- how many indices hold the same name on both sides. It answers "is the
//!   absolute order identical?", which is the question a fingerprinter comparing two lists asks. It is
//!   also BRITTLE BY CONSTRUCTION: move one name from the front of the list to the back and every
//!   position after it shifts by one, so a ONE-NAME defect reports as a 100% difference. See
//!   `one_relocation_destroys_positions_but_costs_lcro_exactly_one` below, which is that case as a test.
//!
//! * `lcro` (longest common relative order) -- the length of the longest subsequence of names that
//!   appears in the same relative order on both sides, i.e. the LCS of the two sequences. Its
//!   complement, `out_of_place = common_len - lcro`, is exactly "the minimum number of names you would
//!   have to pick up and re-insert to make the orders agree". That is a defect COUNT: one misplaced
//!   name costs 1 whether it moved one slot or a thousand.
//!
//! THE GATE RATCHETS `out_of_place`, DOWNWARD ONLY, and PRINTS `positions_agree`. The reason is the
//! brief's own requirement that a ratchet must not go red on a genuine improvement. Suppose a unit
//! inserts a global the bundle has at its correct place: `positions_agree` collapses (everything after
//! the insertion point shifts) while `out_of_place` is unchanged -- the relative order is still as good
//! as it was. A ratchet on `positions_agree` would call that a regression, and the only way to make it
//! green again would be to undo the fix. A ratchet people can only satisfy by reverting improvements is
//! a ratchet people delete.
//!
//! `out_of_place` is always satisfiable honestly, which is the other half of the justification: an
//! installer chooses when it creates a global, so a name added in the wrong slot can always be moved to
//! the right one. The ratchet never asks for something the tree cannot do.
//!
//! BOTH METRICS ARE COMPUTED OVER THE INTERSECTION of the two name sets, never over the raw arrays.
//! A name only one side has has no counterpart to be "in order" with, and including it would let a
//! missing-global defect (already counted, and asserted at zero in the worker realm) masquerade as an
//! ordering defect. `missing` and `extra` are reported separately so the reader can see that the
//! denominator is honest.

use std::collections::{HashMap, HashSet};

/// One name the two orders disagree about, with both of its indices — the diagnostic a unit fixing the
/// order actually needs, since "which names, and how far did each one move" is not recoverable from a
/// count. `bundle_at` and `realm_at` index the intersection sequences, so they are comparable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Displaced {
    pub name: String,
    pub bundle_at: usize,
    pub realm_at: usize,
}

impl std::fmt::Display for Displaced {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:<32} bundle[{:>4}] -> korvus[{:>4}]  ({:+})",
            self.name, self.bundle_at, self.realm_at, self.realm_at as i64 - self.bundle_at as i64)
    }
}

/// The comparison of one realm's live own-global order against the bundle's.
#[derive(Debug, Clone)]
pub struct OrderReport {
    /// `globals.len()` in the bundle artifact for this realm.
    pub baseline_len: usize,
    /// `Object.getOwnPropertyNames(globalThis).length` in the live realm.
    pub observed_len: usize,
    /// The size of the intersection -- the denominator of both metrics.
    pub common_len: usize,
    /// Indices (into the intersection sequences) that hold the same name on both sides.
    pub positions_agree: usize,
    /// The first index where they disagree, as `(index, baseline_name, observed_name)`.
    pub first_divergence: Option<(usize, String, String)>,
    /// Longest common relative order: the LCS length of the two intersection sequences.
    pub lcro: usize,
    /// `common_len - lcro`: the minimum number of names that must be re-inserted to make the two
    /// orders agree. THIS is the ratcheted number.
    pub out_of_place: usize,
    /// A minimal witness set: the names LCS had to drop, in observed order, each with the index it
    /// holds in the bundle's sequence and the index it holds in this realm's. `out_of_place` entries.
    /// Both indices are into the INTERSECTION sequences, so they are directly comparable.
    pub displaced: Vec<Displaced>,
    /// Baseline names this realm does not expose (a SET defect, reported so the denominator is legible).
    pub missing: Vec<String>,
    /// Names this realm exposes that the baseline does not (likewise).
    pub extra: Vec<String>,
}

impl OrderReport {
    /// `lcro / common_len` as a percentage; 100.0 for an empty intersection is not reachable here
    /// because every caller asserts a non-trivial denominator.
    pub fn lcro_pct(&self) -> f64 {
        if self.common_len == 0 { 0.0 } else { 100.0 * self.lcro as f64 / self.common_len as f64 }
    }

    /// `positions_agree / common_len` as a percentage.
    pub fn positions_pct(&self) -> f64 {
        if self.common_len == 0 { 0.0 } else { 100.0 * self.positions_agree as f64 / self.common_len as f64 }
    }

    /// The scoreboard block, identical in shape for every realm so the two are directly comparable.
    pub fn render(&self, realm: &str, sample: usize) -> String {
        let mut s = String::new();
        s.push_str(&format!(
            "\n========== OWN-GLOBAL ORDER: {realm} vs the capture bundle ==========\n"
        ));
        s.push_str(&format!(
            "NAMES        korvus {:>5} / bundle {:>5}   common {:>5}   (missing {}, korvus-only {})\n",
            self.observed_len, self.baseline_len, self.common_len, self.missing.len(), self.extra.len()
        ));
        s.push_str(&format!(
            "POSITIONS  {:>5} of {:>5} agree ({:>5.1}%)   <- printed, NOT ratcheted: one relocation moves every later index\n",
            self.positions_agree, self.common_len, self.positions_pct()
        ));
        s.push_str(&format!(
            "LCRO       {:>5} of {:>5}       ({:>5.1}%)   <- longest common relative order\n",
            self.lcro, self.common_len, self.lcro_pct()
        ));
        s.push_str(&format!(
            "OUT-OF-PLACE {:>3}                            <- THE RATCHET (common - lcro; lower is better)\n",
            self.out_of_place
        ));
        match &self.first_divergence {
            Some((i, b, k)) => s.push_str(&format!(
                "FIRST DIVERGENCE at index {i}: bundle {b:?}, korvus {k:?}\n"
            )),
            None => s.push_str("FIRST DIVERGENCE none -- the common order is identical\n"),
        }
        s.push_str(&format!("\n-- DISPLACED (a minimal set of names to re-insert) (showing {}/{}) --\n",
            sample.min(self.displaced.len()), self.displaced.len()));
        for d in self.displaced.iter().take(sample) {
            s.push_str(&format!("   {d}\n"));
        }
        let show = |s: &mut String, label: &str, v: &[String]| {
            s.push_str(&format!("\n-- {label} (showing {}/{}) --\n", sample.min(v.len()), v.len()));
            for x in v.iter().take(sample) {
                s.push_str(&format!("   {x}\n"));
            }
        };
        show(&mut s, "MISSING (bundle names this realm lacks)", &self.missing);
        show(&mut s, "KORVUS-ONLY (names the bundle does not have)", &self.extra);
        s
    }
}

/// Compare a live realm's own-global order against the bundle's.
///
/// Both inputs must be duplicate-free -- they come from `Object.getOwnPropertyNames`, which cannot
/// repeat a key, and a duplicate would silently break the permutation argument the LCS rests on. It is
/// asserted rather than tolerated: a duplicate here means the dumper is broken, not that the realm is.
pub fn compare(baseline: &[&str], observed: &[&str]) -> OrderReport {
    let mut base_index: HashMap<&str, usize> = HashMap::with_capacity(baseline.len());
    for (i, n) in baseline.iter().enumerate() {
        assert!(base_index.insert(n, i).is_none(), "duplicate name in the baseline global list: {n:?}");
    }
    let obs_set: HashSet<&str> = observed.iter().copied().collect();
    assert_eq!(obs_set.len(), observed.len(), "duplicate name in the observed global list");

    // The two intersection sequences. `common_observed` is the observed order restricted to shared
    // names; `seq` is each of those names' index in the baseline, so an ASCENDING run of `seq` is
    // precisely a run of names the two sides agree on the relative order of.
    let common_observed: Vec<&str> = observed.iter().copied().filter(|n| base_index.contains_key(n)).collect();
    let common_baseline: Vec<&str> = baseline.iter().copied().filter(|n| obs_set.contains(n)).collect();
    debug_assert_eq!(common_observed.len(), common_baseline.len());
    let seq: Vec<usize> = common_observed.iter().map(|n| base_index[n]).collect();

    let positions_agree = common_baseline
        .iter()
        .zip(common_observed.iter())
        .filter(|(b, k)| b == k)
        .count();
    let first_divergence = common_baseline
        .iter()
        .zip(common_observed.iter())
        .enumerate()
        .find(|(_, (b, k))| b != k)
        .map(|(i, (b, k))| (i, b.to_string(), k.to_string()));

    let keep = longest_increasing_subsequence(&seq);
    let kept: HashSet<usize> = keep.iter().copied().collect();
    // `bundle_at` is the name's index in the COMMON baseline sequence, not in the raw baseline: the two
    // must share a denominator or the "(+n)" drift column would be measuring the missing names too.
    let common_base_index: HashMap<&str, usize> =
        common_baseline.iter().enumerate().map(|(i, n)| (*n, i)).collect();
    let displaced: Vec<Displaced> = common_observed
        .iter()
        .enumerate()
        .filter(|(i, _)| !kept.contains(i))
        .map(|(i, n)| Displaced { name: n.to_string(), bundle_at: common_base_index[n], realm_at: i })
        .collect();

    let mut missing: Vec<String> =
        baseline.iter().filter(|n| !obs_set.contains(*n)).map(|n| n.to_string()).collect();
    let mut extra: Vec<String> =
        observed.iter().filter(|n| !base_index.contains_key(*n)).map(|n| n.to_string()).collect();
    missing.sort();
    extra.sort();

    OrderReport {
        baseline_len: baseline.len(),
        observed_len: observed.len(),
        common_len: common_observed.len(),
        positions_agree,
        first_divergence,
        lcro: keep.len(),
        out_of_place: common_observed.len() - keep.len(),
        displaced,
        missing,
        extra,
    }
}

/// Indices of a longest STRICTLY increasing subsequence of `seq`, ascending.
///
/// `seq` holds the baseline index of each shared name in observed order, so it is a permutation of a
/// set of distinct integers; for two sequences that are permutations of one another the LCS is exactly
/// this LIS, which is why the O(n log n) patience algorithm is admissible where a general O(n*m) LCS
/// would be 1.3 M cells for the window realm. Deterministic: `partition_point` breaks every tie the
/// same way, so `displaced` is the same witness set on every run of the same input.
fn longest_increasing_subsequence(seq: &[usize]) -> Vec<usize> {
    if seq.is_empty() {
        return Vec::new();
    }
    // tails[k] = the index into `seq` of the smallest tail among increasing subsequences of length k+1.
    let mut tails: Vec<usize> = Vec::new();
    let mut prev: Vec<Option<usize>> = vec![None; seq.len()];
    for i in 0..seq.len() {
        let pos = tails.partition_point(|&t| seq[t] < seq[i]);
        prev[i] = if pos == 0 { None } else { Some(tails[pos - 1]) };
        if pos == tails.len() {
            tails.push(i);
        } else {
            tails[pos] = i;
        }
    }
    let mut out = Vec::with_capacity(tails.len());
    let mut cur = Some(*tails.last().expect("non-empty"));
    while let Some(i) = cur {
        out.push(i);
        cur = prev[i];
    }
    out.reverse();
    out
}

/// Read a realm's `globals` array out of a surface artifact (the bundle's, or a live dump in the same
/// shape). One reader, so the bundle side and the live side cannot drift apart in how they are parsed.
pub fn globals_of(surface: &serde_json::Value) -> Vec<String> {
    surface["globals"]
        .as_array()
        .unwrap_or_else(|| panic!("surface JSON has no `globals` array"))
        .iter()
        .map(|v| v.as_str().expect("a global name is a string").to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmp(b: &[&str], k: &[&str]) -> OrderReport {
        compare(b, k)
    }

    // ── THE SYNTHETIC CASES ────────────────────────────────────────────────────────
    //
    // A ratchet nobody can predict is a ratchet people learn to re-baseline blindly, so each of the
    // four perturbations the brief names gets a test that PINS what both metrics do to it. Read these
    // as the metric's specification: if one of them changes, the meaning of the gate's number changed.

    #[test]
    fn identical_orders_score_perfect_on_both_metrics() {
        let r = cmp(&["a", "b", "c", "d", "e"], &["a", "b", "c", "d", "e"]);
        assert_eq!((r.common_len, r.positions_agree, r.lcro, r.out_of_place), (5, 5, 5, 0));
        assert!(r.first_divergence.is_none());
        assert!(r.displaced.is_empty());
    }

    /// CASE 1 -- REORDER TWO NAMES (adjacent). Both metrics notice, and they disagree by design about
    /// how bad it is: positions says two slots are wrong, LCRO says one name is misplaced. Both are
    /// true statements about the same swap.
    #[test]
    fn swapping_two_adjacent_names_costs_two_positions_and_one_lcro() {
        let r = cmp(&["a", "b", "c", "d", "e"], &["a", "c", "b", "d", "e"]);
        assert_eq!(r.positions_agree, 3); // a, d, e
        assert_eq!(r.lcro, 4); // drop either b or c
        assert_eq!(r.out_of_place, 1);
        assert_eq!(r.first_divergence, Some((1, "b".into(), "c".into())));
    }

    /// CASE 1b -- REORDER TWO NAMES (distant). `positions_agree` cannot tell this from the adjacent
    /// swap; LCRO can, because crossing a long span breaks more of the relative order.
    #[test]
    fn swapping_two_distant_names_costs_the_same_positions_but_more_lcro() {
        let r = cmp(&["a", "b", "c", "d", "e"], &["a", "e", "c", "d", "b"]);
        assert_eq!(r.positions_agree, 3); // identical to the adjacent swap above
        assert_eq!(r.lcro, 3); // a, c, d
        assert_eq!(r.out_of_place, 2); // strictly worse than the adjacent swap -- correct
    }

    /// CASE 2 -- INSERT ONE NAME the baseline does not have. It is a SET defect (`extra`), not an
    /// ordering one, so it must not move either metric: the intersection is unchanged.
    #[test]
    fn inserting_a_korvus_only_name_is_a_set_defect_and_moves_neither_metric() {
        let r = cmp(&["a", "b", "c", "d", "e"], &["a", "zz", "b", "c", "d", "e"]);
        assert_eq!(r.extra, vec!["zz".to_string()]);
        assert_eq!((r.common_len, r.positions_agree, r.lcro, r.out_of_place), (5, 5, 5, 0));
    }

    /// CASE 3 -- REMOVE ONE NAME. Also a SET defect: the denominator shrinks by one and the order of
    /// what is left is still perfect. This is the case that makes the intersection the right domain --
    /// scored against the raw baseline, a missing name would shift every later index and read as an
    /// ordering collapse, double-counting a defect `MISSING-GLOB` already owns.
    #[test]
    fn removing_a_name_shrinks_the_denominator_and_leaves_the_order_perfect() {
        let r = cmp(&["a", "b", "c", "d", "e"], &["a", "b", "d", "e"]);
        assert_eq!(r.missing, vec!["c".to_string()]);
        assert_eq!((r.common_len, r.positions_agree, r.lcro, r.out_of_place), (4, 4, 4, 0));
    }

    /// CASE 3b -- and then ADDING THAT NAME BACK IN THE WRONG PLACE costs exactly one. This pair is the
    /// honest answer to "can a presence fix turn the ratchet red?": yes, and it should, because the
    /// name really is in the wrong slot -- and the fix is to insert it at the right index, which an
    /// installer can always do.
    #[test]
    fn re_adding_a_missing_name_in_the_wrong_slot_costs_exactly_one_out_of_place() {
        let r = cmp(&["a", "b", "c", "d", "e"], &["a", "b", "d", "e", "c"]);
        assert!(r.missing.is_empty());
        assert_eq!((r.common_len, r.lcro, r.out_of_place), (5, 4, 1));
    }

    /// CASE 4 -- THE ONE THAT DECIDED THE RATCHET. Relocating a SINGLE name from the front to the back
    /// shifts every other index by one: `positions_agree` reads 0 of 5, a total loss, for a defect that
    /// is one name wide. `out_of_place` reads 1. Ratcheting the first number would mean a one-name
    /// change could never be distinguished from a wholesale reordering.
    #[test]
    fn one_relocation_destroys_positions_but_costs_lcro_exactly_one() {
        let r = cmp(&["a", "b", "c", "d", "e"], &["b", "c", "d", "e", "a"]);
        assert_eq!(r.positions_agree, 0);
        assert_eq!(r.positions_pct(), 0.0);
        assert_eq!(r.lcro, 4);
        assert_eq!(r.out_of_place, 1);
        // The witness names the one name AND how far it moved: first slot to last.
        assert_eq!(r.displaced, vec![Displaced { name: "a".into(), bundle_at: 0, realm_at: 4 }]);
    }

    /// CASE 4b -- the mirror, which is the improvement case the ratchet must NOT punish: inserting a
    /// name at the front where the baseline also has it at the front. Perfect on both metrics; the
    /// point is that the same shift-everything mechanics that wrecked `positions_agree` above are
    /// harmless when the move was correct, and only LCRO reports that correctly in both directions.
    #[test]
    fn a_correct_insertion_at_the_front_is_free_on_the_ratcheted_metric() {
        let before = cmp(&["a", "b", "c", "d", "e"], &["b", "c", "d", "e"]);
        let after = cmp(&["a", "b", "c", "d", "e"], &["a", "b", "c", "d", "e"]);
        assert_eq!(before.out_of_place, 0);
        assert_eq!(after.out_of_place, 0); // the ratchet does not move: nothing got worse
        assert_eq!(after.lcro, 5); // and LCRO records that something got better
    }

    /// The ratchet must be MONOTONE in the obvious direction, or "lower is better" is a slogan rather
    /// than a property. Reversing the whole list is the worst case: LCRO is 1 (any single name is a
    /// common subsequence of length 1) and `out_of_place` is n-1.
    #[test]
    fn a_fully_reversed_order_is_the_worst_case() {
        let r = cmp(&["a", "b", "c", "d", "e"], &["e", "d", "c", "b", "a"]);
        assert_eq!(r.lcro, 1);
        assert_eq!(r.out_of_place, 4);
    }

    /// Same input twice, same answer -- including the witness list, which is the part a tie-breaking
    /// change could quietly make unstable.
    #[test]
    fn the_metric_is_deterministic() {
        let b = ["a", "b", "c", "d", "e", "f", "g", "h"];
        let k = ["c", "a", "b", "f", "d", "h", "e", "g"];
        let (r1, r2) = (cmp(&b, &k), cmp(&b, &k));
        assert_eq!(r1.lcro, r2.lcro);
        assert_eq!(r1.out_of_place, r2.out_of_place);
        assert_eq!(r1.positions_agree, r2.positions_agree);
        assert_eq!(r1.displaced, r2.displaced);
    }

    #[test]
    fn disjoint_name_sets_have_no_order_to_compare() {
        let r = cmp(&["a", "b"], &["x", "y"]);
        assert_eq!((r.common_len, r.lcro, r.out_of_place), (0, 0, 0));
        assert_eq!(r.missing, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(r.extra, vec!["x".to_string(), "y".to_string()]);
    }

    #[test]
    fn globals_of_reads_the_surface_shape() {
        let v: serde_json::Value = serde_json::json!({ "globals": ["Object", "Function"] });
        assert_eq!(globals_of(&v), vec!["Object".to_string(), "Function".to_string()]);
    }
}
