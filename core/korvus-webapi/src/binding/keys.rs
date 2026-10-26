//! `binding/keys.rs` — the memo in front of the engine's per-name V8 keys (private keys, event slot keys,
//! property-name strings).
//!
//! The engine names its keys with string literals, so one name keeps coming from one address: a memo
//! indexed by that address answers a hit with a pointer comparison's worth of work and a byte compare,
//! where the name-keyed maps behind it SipHash the whole name (`private_key` + `hash_one` + `Sip13` were
//! 25.8 % of a `childNodes[i]` loop, profiled 2026-10-23).

/// A four-way memo by where a name's bytes are: a name takes [`entry_of`](Self::entry_of), or else the first free
/// entry of its group of [`WAYS`] (looked at from its own on). A hit is confirmed by comparing the bytes, so an
/// address another string reuses only misses. An entry is filled once and kept: a fifth name on one group takes
/// its turn missing (into the map behind it), never evicting the four at an allocation per call.
///
/// It was direct-mapped, and which literals share an entry moves with every build's layout: on 2026-10-23 a
/// rebuild put `korvus_dtl_cache_class` on the entry `korvus_intrinsics` fills at bootstrap, and every
/// `el.classList` read missed into the SipHash map (1445 -> 1792 instructions a read, r2/l3/perf). Of the 257
/// private-key literals placed in that binary, 46-55 (by where ASLR puts the image) found their entry taken by
/// another, each a cliff for whichever came second; four ways over 1024 entries leave 0-3 (r2/l3/r1/perf/memo3.py).
pub(crate) struct AddrMemo<V, const N: usize> {
    at: Vec<Option<(Box<str>, V)>>,
}

impl<V, const N: usize> Default for AddrMemo<V, N> {
    fn default() -> Self {
        AddrMemo { at: (0..N).map(|_| None).collect() }
    }
}

/// How many entries a name may take ([`AddrMemo`]); `N` is a multiple of it.
const WAYS: usize = 4;

/// The entries of `i`'s group, `i` first.
#[inline]
fn ways(i: usize) -> impl Iterator<Item = usize> {
    let group = i & !(WAYS - 1);
    (0..WAYS).map(move |k| group | ((i + k) & (WAYS - 1)))
}

impl<V, const N: usize> AddrMemo<V, N> {
    #[cfg(test)]
    pub(crate) const ENTRIES: usize = N;
    const WHOLE_GROUPS: () = assert!(N % WAYS == 0);

    pub(crate) fn entry_of(name: &str) -> usize {
        let at = name.as_ptr() as usize;
        (at ^ (at >> 6)) % N
    }

    pub(crate) fn get(&self, i: usize, name: &str) -> Option<&V> {
        for j in ways(i) {
            match &self.at[j] {
                Some((n, v)) if **n == *name => return Some(v),
                _ => {}
            }
        }
        None
    }

    /// Keep `v` for `name` in a free way of entry `i`'s group: `false` when the group is full (nothing kept).
    pub(crate) fn fill(&mut self, i: usize, name: &str, v: impl FnOnce() -> V) -> bool {
        let () = Self::WHOLE_GROUPS;
        match ways(i).find(|&j| self.at[j].is_none()) {
            Some(e) => {
                self.at[e] = Some((name.into(), v()));
                true
            }
            None => false,
        }
    }
}

/// Whether `name` may take a memo entry: only a name whose bytes lie in the binary's image, a literal of
/// the engine's. A name built per call (a `format!`, a name a page chose) sits at a heap address it will
/// not come from again, so filling an entry with it would only keep a literal that maps there out of it
/// for the isolate's life. Where the image cannot be read (`/proc` is Linux's), any name fills, as the
/// event slot memo always did; a hit is byte-confirmed either way.
pub(crate) fn fillable(name: &str) -> bool {
    if !crate::install::templates::image_known() {
        return true;
    }
    crate::install::templates::in_image(name.as_ptr() as usize)
}

#[cfg(test)]
mod tests {
    use super::{fillable, AddrMemo};

    #[test]
    fn a_hit_needs_the_same_bytes_and_an_entry_is_filled_once() {
        let mut m = AddrMemo::<u32, 64>::default();
        m.fill(3, "cancelable", || 1);
        assert_eq!(m.get(3, "cancelable"), Some(&1));
        assert_eq!(m.get(2, "cancelable"), Some(&1), "another entry of the group finds it too");
        assert_eq!(m.get(3, "defaultPrevented"), None, "another name on the same entry misses");
        assert_eq!(m.get(3, "cancelabl"), None);
        m.fill(3, "defaultPrevented", || 2);
        assert_eq!(m.get(3, "cancelable"), Some(&1), "the first name keeps its entry");
        assert_eq!(m.get(3, "defaultPrevented"), Some(&2), "the second takes another entry of the group");
        let owned = String::from("cancelable");
        assert_eq!(m.get(3, &owned), Some(&1), "the same bytes from another address");
        assert_eq!(m.get(4, "cancelable"), None);
    }

    /// Two literals on one entry both hit, whichever filled first: the cliff a direct-mapped memo had (the
    /// classList key behind the bootstrap's `korvus_intrinsics`, 2026-10-23).
    #[test]
    fn two_names_on_one_entry_both_hit() {
        let mut m = AddrMemo::<u32, 512>::default();
        m.fill(40, "korvus_intrinsics", || 1);
        m.fill(40, "korvus_dtl_cache_class", || 2);
        assert_eq!(m.get(40, "korvus_intrinsics"), Some(&1));
        assert_eq!(m.get(40, "korvus_dtl_cache_class"), Some(&2));
        let mut m = AddrMemo::<u32, 512>::default();
        m.fill(41, "korvus_dtl_cache_class", || 2);
        m.fill(40, "korvus_intrinsics", || 1);
        assert_eq!(m.get(40, "korvus_intrinsics"), Some(&1));
        assert_eq!(m.get(41, "korvus_dtl_cache_class"), Some(&2));
    }

    #[test]
    fn every_name_has_an_entry() {
        let long = "x".repeat(4096);
        for name in ["", "type", "cancelable", "defaultPrevented", "_stopImmediate", "ünïcödé — 名前", &"x".repeat(100), &long] {
            assert!(AddrMemo::<u32, 64>::entry_of(name) < AddrMemo::<u32, 64>::ENTRIES);
            assert!(AddrMemo::<u32, 512>::entry_of(name) < AddrMemo::<u32, 512>::ENTRIES);
        }
    }

    #[test]
    fn a_literal_fills_and_a_heap_copy_only_hits() {
        const NAME: &str = "korvus_list_class";
        let heap = String::from(NAME);
        if crate::install::templates::image_known() {
            assert!(fillable(NAME), "a literal of the binary fills");
            assert!(!fillable(&heap), "a heap string never fills");
        }
        let mut m = AddrMemo::<u32, 512>::default();
        let i = AddrMemo::<u32, 512>::entry_of(NAME);
        if fillable(NAME) {
            m.fill(i, NAME, || 7);
        }
        assert_eq!(m.get(i, &heap), Some(&7), "the same bytes from the heap hit the literal's entry");
        let j = AddrMemo::<u32, 512>::entry_of(&heap);
        if fillable(&heap) {
            m.fill(j, &heap, || 8);
        }
        if crate::install::templates::image_known() {
            assert_eq!(m.get(j, &heap), if i / 4 == j / 4 { Some(&7) } else { None }, "a heap name took no entry of its own");
        }
    }

    #[test]
    fn a_fifth_name_on_a_group_misses_and_never_evicts() {
        let mut m = AddrMemo::<u32, 512>::default();
        for (k, name) in ["first", "second", "third", "fourth"].into_iter().enumerate() {
            m.fill(9, name, || k as u32);
        }
        m.fill(10, "fifth", || 5);
        assert_eq!(m.get(9, "fifth"), None);
        assert_eq!(m.get(10, "fifth"), None);
        assert_eq!(m.get(11, "first"), Some(&0));
        assert_eq!(m.get(8, "fourth"), Some(&3));
        assert_eq!(m.get(12, "first"), None, "the next group is another");
    }
}
