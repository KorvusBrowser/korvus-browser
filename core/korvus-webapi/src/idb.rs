//! Native IndexedDB core (surface-fidelity work, LIES 92->47; history in git). This module is
//! the pure-Rust correctness core the whole subsystem rests on: the IndexedDB KEY model + total order
//! (the spec's type ordering number<date<string<binary<array), key-path extraction, and range
//! predicates. It has NO v8 dependency, so it is unit-tested in isolation before the cppgc classes +
//! async request queue (bindings.rs) are built on top. Record VALUES are structured-clone JS
//! (v8::Global) held by the classes; only KEYS are this Rust enum, extracted at the JS boundary.

// Foundational core: consumed by the IDB cppgc classes (bindings.rs) in later steps of the port.
#![allow(dead_code)]

use std::cmp::Ordering;

/// A valid IndexedDB key. Invalid keys (NaN number, invalid Date, non-key types) are never
/// constructed -- the boundary extractor returns `None` for them (the spec's `DataError`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Key {
    Number(f64),
    /// A Date key, stored as its epoch-millis (getTime); ordered after all numbers.
    Date(f64),
    String(String),
    Binary(Vec<u8>),
    Array(Vec<Key>),
}

impl Key {
    /// The spec type rank: number(0) < date(1) < string(2) < binary(3) < array(4).
    fn rank(&self) -> u8 {
        match self {
            Key::Number(_) => 0,
            Key::Date(_) => 1,
            Key::String(_) => 2,
            Key::Binary(_) => 3,
            Key::Array(_) => 4,
        }
    }

    /// Total order over valid keys (the IndexedDB key-comparison algorithm).
    pub fn cmp(&self, other: &Key) -> Ordering {
        let (ra, rb) = (self.rank(), other.rank());
        if ra != rb {
            return ra.cmp(&rb);
        }
        match (self, other) {
            // Valid Number/Date keys are never NaN, so partial_cmp is total here.
            (Key::Number(a), Key::Number(b)) | (Key::Date(a), Key::Date(b)) => {
                a.partial_cmp(b).unwrap_or(Ordering::Equal)
            }
            (Key::String(a), Key::String(b)) => a.cmp(b),
            (Key::Binary(a), Key::Binary(b)) => a.cmp(b), // Vec<u8> lexicographic = byte order + shorter-first
            (Key::Array(a), Key::Array(b)) => {
                let n = a.len().min(b.len());
                for i in 0..n {
                    let c = a[i].cmp(&b[i]);
                    if c != Ordering::Equal {
                        return c;
                    }
                }
                a.len().cmp(&b.len())
            }
            _ => unreachable!("rank equality guarantees same variant"),
        }
    }
}

/// A normalized query: an inclusive/exclusive key range, or unbounded (`null`/`undefined` query).
#[derive(Debug, Clone, Default)]
pub struct KeyRange {
    pub lower: Option<Key>,
    pub upper: Option<Key>,
    pub lower_open: bool,
    pub upper_open: bool,
}

impl KeyRange {
    /// Unbounded range (matches every key) -- the `toRange(null)` case.
    pub fn all() -> KeyRange {
        KeyRange { lower: None, upper: None, lower_open: true, upper_open: true }
    }
    /// A single-key range (`toRange(key)` / `IDBKeyRange.only`).
    pub fn only(key: Key) -> KeyRange {
        KeyRange { lower: Some(key.clone()), upper: Some(key), lower_open: false, upper_open: false }
    }
    /// `IDBKeyRange.bound`; `Err` on an invalid/empty range (the spec's `DataError`).
    pub fn bound(lower: Key, upper: Key, lower_open: bool, upper_open: bool) -> Result<KeyRange, &'static str> {
        match lower.cmp(&upper) {
            Ordering::Greater => Err("lower > upper"),
            Ordering::Equal if lower_open || upper_open => Err("empty range"),
            _ => Ok(KeyRange { lower: Some(lower), upper: Some(upper), lower_open, upper_open }),
        }
    }
    pub fn includes(&self, key: &Key) -> bool {
        if let Some(l) = &self.lower {
            match key.cmp(l) {
                Ordering::Less => return false,
                Ordering::Equal if self.lower_open => return false,
                _ => {}
            }
        }
        if let Some(u) = &self.upper {
            match key.cmp(u) {
                Ordering::Greater => return false,
                Ordering::Equal if self.upper_open => return false,
                _ => {}
            }
        }
        true
    }
}

/// Insertion point that keeps `records` sorted by key (first index whose key is > `key`).
pub fn insert_pos(keys: &[Key], key: &Key) -> usize {
    keys.iter().position(|k| k.cmp(key) == Ordering::Greater).unwrap_or(keys.len())
}
/// Index of an exact key match in a sorted list, or `None`.
pub fn exact_pos(keys: &[Key], key: &Key) -> Option<usize> {
    for (i, k) in keys.iter().enumerate() {
        match k.cmp(key) {
            Ordering::Equal => return Some(i),
            Ordering::Greater => return None,
            Ordering::Less => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(x: &str) -> Key { Key::String(x.to_string()) }
    fn n(x: f64) -> Key { Key::Number(x) }

    #[test]
    fn type_ordering_number_lt_date_lt_string_lt_binary_lt_array() {
        let ordered = [
            n(999.0),
            Key::Date(0.0),
            s("aaa"),
            Key::Binary(vec![0]),
            Key::Array(vec![n(0.0)]),
        ];
        for i in 0..ordered.len() {
            for j in 0..ordered.len() {
                assert_eq!(ordered[i].cmp(&ordered[j]), i.cmp(&j), "rank order {i} vs {j}");
            }
        }
    }

    #[test]
    fn within_type_ordering() {
        assert_eq!(n(1.0).cmp(&n(2.0)), Ordering::Less);
        assert_eq!(s("a").cmp(&s("b")), Ordering::Less);
        assert_eq!(s("ab").cmp(&s("a")), Ordering::Greater); // longer, shares prefix
        assert_eq!(Key::Binary(vec![1, 2]).cmp(&Key::Binary(vec![1, 3])), Ordering::Less);
        assert_eq!(Key::Binary(vec![1]).cmp(&Key::Binary(vec![1, 0])), Ordering::Less); // shorter first
    }

    #[test]
    fn array_ordering_is_elementwise_then_length() {
        let a = Key::Array(vec![n(1.0), n(2.0)]);
        let b = Key::Array(vec![n(1.0), n(3.0)]);
        let c = Key::Array(vec![n(1.0)]);
        assert_eq!(a.cmp(&b), Ordering::Less);
        assert_eq!(a.cmp(&c), Ordering::Greater); // shares prefix, longer
        assert_eq!(a.cmp(&a), Ordering::Equal);
    }

    #[test]
    fn range_includes_open_closed() {
        let r = KeyRange::bound(n(1.0), n(3.0), false, true).unwrap(); // [1, 3)
        assert!(r.includes(&n(1.0)));
        assert!(r.includes(&n(2.0)));
        assert!(!r.includes(&n(3.0))); // upper open
        assert!(!r.includes(&n(0.0)));
        assert!(KeyRange::all().includes(&s("anything")));
        assert!(KeyRange::only(n(5.0)).includes(&n(5.0)));
        assert!(!KeyRange::only(n(5.0)).includes(&n(6.0)));
        assert!(KeyRange::bound(n(1.0), n(1.0), true, false).is_err()); // empty (lowerOpen, equal)
        assert!(KeyRange::bound(n(3.0), n(1.0), false, false).is_err()); // lower > upper
    }

    #[test]
    fn sorted_positions() {
        let keys = [n(1.0), n(3.0), n(5.0)];
        assert_eq!(insert_pos(&keys, &n(4.0)), 2);
        assert_eq!(insert_pos(&keys, &n(0.0)), 0);
        assert_eq!(insert_pos(&keys, &n(9.0)), 3);
        assert_eq!(exact_pos(&keys, &n(3.0)), Some(1));
        assert_eq!(exact_pos(&keys, &n(4.0)), None);
    }
}
