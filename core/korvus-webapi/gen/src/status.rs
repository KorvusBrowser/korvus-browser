//! `webapi status` — one dashboard across interfaces (P5).
//!
//! Merges the two coverage dimensions into a single per-family table: STATIC impl% (from
//! coverage.rs, scanning the behavior scaffold source — "does the code exist?") and, when a WPT
//! dump is supplied, WPT pass-rate (from wpt.rs bucketing — "does it work?"), plus the
//! next-to-implement stubs. Coverage is keyed by exact interface (DOMRect, DOMRectReadOnly); WPT is
//! keyed by the base name a test file exercises (DOMRect covers both). So rows are grouped by
//! **family** (`family_base`) — DOMRect + DOMRectReadOnly share a row — which lines both up.

use std::collections::BTreeMap;

use crate::coverage::Coverage;
/// The family base name: strip a trailing `ReadOnly` so a base and its read-only variant group
/// together (`DOMRectReadOnly` -> `DOMRect`). Was `layout::family_base` until the scaffold went.
fn family_base(iface: &str) -> String {
    iface.strip_suffix("ReadOnly").unwrap_or(iface).to_string()
}
use crate::wpt::Bucket;

/// One dashboard row: an interface family, its implemented/declared member counts, its WPT bucket
/// (if a dump was given), and the still-stubbed members (next-to-implement).
#[derive(Debug, Clone)]
pub struct StatusRow {
    pub family: String,
    pub impl_count: usize,
    pub decl_count: usize,
    pub wpt: Option<Bucket>,
    pub missing: Vec<String>,
}

impl StatusRow {
    pub fn impl_pct(&self) -> f64 {
        if self.decl_count == 0 { 0.0 } else { 100.0 * self.impl_count as f64 / self.decl_count as f64 }
    }
}

/// Build the dashboard rows: group static coverage by family, attach the family's WPT bucket.
pub fn build(cov: &Coverage, wpt: Option<&BTreeMap<String, Bucket>>) -> Vec<StatusRow> {
    let mut fams: BTreeMap<String, (usize, usize, Vec<String>)> = BTreeMap::new();
    for (iface, ic) in &cov.interfaces {
        if ic.decl_count() == 0 {
            continue; // an impl with no `<Iface>Behavior` trait — not a scaffolded interface
        }
        let entry = fams.entry(family_base(iface)).or_default();
        entry.0 += ic.impl_count();
        entry.1 += ic.decl_count();
        entry.2.extend(ic.missing().iter().map(|s| s.to_string()));
    }
    fams.into_iter()
        .map(|(family, (impl_count, decl_count, mut missing))| {
            missing.sort();
            StatusRow {
                wpt: wpt.and_then(|w| w.get(&family).cloned()),
                family,
                impl_count,
                decl_count,
                missing,
            }
        })
        .collect()
}

/// Render the dashboard as a table (worst WPT-rate first when WPT data is present, else by name).
pub fn render(rows: &[StatusRow]) -> String {
    let mut rows: Vec<&StatusRow> = rows.iter().collect();
    if rows.iter().any(|r| r.wpt.is_some()) {
        rows.sort_by(|a, b| {
            let ra = a.wpt.as_ref().and_then(|w| w.rate()).unwrap_or(f64::INFINITY);
            let rb = b.wpt.as_ref().and_then(|w| w.rate()).unwrap_or(f64::INFINITY);
            ra.partial_cmp(&rb).unwrap().then(a.family.cmp(&b.family))
        });
    }

    let mut out = String::new();
    out.push_str(&format!("{:<24} {:>9} {:>7} {:>10}  next-to-implement\n", "interface family", "impl", "impl%", "wpt"));
    out.push_str(&"-".repeat(72));
    out.push('\n');
    for r in &rows {
        let impl_s = format!("{}/{}", r.impl_count, r.decl_count);
        let wpt_s = match &r.wpt {
            Some(b) => format!("{}/{} {:.0}%", b.passed, b.total, b.rate().unwrap_or(0.0)),
            None => "-".to_string(),
        };
        let next = if r.missing.is_empty() {
            "—".to_string()
        } else {
            let head: Vec<&str> = r.missing.iter().take(3).map(String::as_str).collect();
            let more = if r.missing.len() > 3 { format!(" +{}", r.missing.len() - 3) } else { String::new() };
            format!("{} stub(s): {}{}", r.missing.len(), head.join(", "), more)
        };
        out.push_str(&format!("{:<24} {:>9} {:>6.0}% {:>10}  {}\n", r.family, impl_s, r.impl_pct(), wpt_s, next));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{coverage, wpt};
    use std::path::Path;

    fn geometry_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/core/geometry")
    }

    /// Static coverage grouped by family lines up with WPT buckets: DOMRect + DOMRectReadOnly share
    /// the DOMRect row (21/21 members) and carry the DOMRect WPT bucket (54/62).
    #[test]
    fn status_groups_by_family_and_merges_wpt() {
        let cov = coverage::scan_dir(&geometry_dir()).expect("scan geometry");
        let files = vec![
            wpt::FileResult { path: "css/geometry/DOMRect-001.html".into(), passed: 26, total: 30, subtests: Vec::new(), failures: Vec::new() },
            wpt::FileResult { path: "css/geometry/DOMRect-002.html".into(), passed: 28, total: 32, subtests: Vec::new(), failures: Vec::new() },
        ];
        let buckets = wpt::bucket(&files);
        let rows = build(&cov, Some(&buckets));

        let dr = rows.iter().find(|r| r.family == "DOMRect").expect("DOMRect family row");
        assert_eq!((dr.impl_count, dr.decl_count), (21, 21), "DOMRect(10) + DOMRectReadOnly(11)");
        assert_eq!(dr.wpt.as_ref().map(|b| (b.passed, b.total)), Some((54, 62)));
        assert!(dr.missing.is_empty(), "geometry is fully implemented");

        // A family with no WPT dump entry has None (DOMMatrix wasn't in the bucket above).
        let dm = rows.iter().find(|r| r.family == "DOMMatrix").expect("DOMMatrix row");
        // DOMMatrix(60) + DOMMatrixReadOnly(46); 103 before DOMMatrix gained its three *Self methods.
        assert_eq!((dm.impl_count, dm.decl_count), (106, 106));
        assert!(dm.wpt.is_none());
    }

    #[test]
    fn render_emits_a_table_with_families_and_percents() {
        let cov = coverage::scan_dir(&geometry_dir()).expect("scan");
        let table = render(&build(&cov, None));
        assert!(table.contains("interface family"));
        assert!(table.contains("DOMRect"));
        assert!(table.contains("100%"), "geometry families are fully implemented");
    }
}
