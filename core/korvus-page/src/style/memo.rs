//! The bounds of the style-write memos: the answers a style write keeps by its inputs' text (`supports`'s recent
//! answers, `inline`'s declaration entries, parsed and written blocks, the CSSOM setter's checks).
//!
//! Each was bounded by its count of entries alone, and each entry keeps whole value texts: a page writing large
//! distinct values -- `style.backgroundImage = 'url(data:...' + i + 100 KB + ')'` a thousand times -- kept every one
//! of them in four memos at once until each reached its count, 880 MB at its peak (`mem1.html`, 2026-10-23;
//! followup-issues.json[9]). A memo whose entries would pass [`BYTES`] is cleared first, as a full one is, and an
//! entry larger than [`ENTRY_BYTES`] -- a quarter of that -- is not kept in it: a page writing 1 MB values keeps none
//! of them, one writing 100 KB values at most a handful.
//!
//! The entry bound is a fraction of the memo's, not a small value's size: at 16 KiB no value of more than about 8 KB
//! was kept (its text counted twice, the key and the answer), and every read of a `style` attribute holding a 20 KB
//! data URL parsed it again -- 2.2M instructions where the base answered in 17k, an observable 60-140x
//! (`ops4.html`, `vparsed.html`, 2026-10-23). The parsed attributes' blocks are kept by text only up to
//! `inline::SHARED_BYTES`: a larger one is its element's (`CSSStyleDeclaration`'s slots, bounded by [`ELEMENT_BYTES`]),
//! so an unchanged attribute of any size is read again without a parse, however many of them a page reads in turn.

/// The most the elements' own inline blocks (`CSSStyleDeclaration`'s slots) keep in all, in bytes of the texts they
/// hold. Blink keeps every element's block while the element lives; the slots outlive their elements (nothing tells
/// them one is gone), so they are cleared at this bound -- a page making element after element with a 1 MB `style`
/// keeps at most this much of them. One slot larger than it is still kept, alone.
pub const ELEMENT_BYTES: usize = 16 * BYTES;

/// The largest entry a memo keeps, in bytes of the texts it holds.
pub const ENTRY_BYTES: usize = BYTES / 4;

/// The most a memo keeps in all, in bytes of the texts it holds.
pub const BYTES: usize = 1 << 20;

/// Make room for an entry of `cost` bytes in a memo of `len` entries, at most `max_len`, holding `bytes` bytes:
/// `false` when the entry is too large to keep; otherwise `true`, `clear` called first when the entry would take
/// the memo past either bound (and `bytes` restarted), and the entry counted.
pub fn admit(len: usize, max_len: usize, bytes: &std::cell::Cell<usize>, cost: usize, clear: impl FnOnce()) -> bool {
    if cost > ENTRY_BYTES {
        return false;
    }
    if len >= max_len || bytes.get() + cost > BYTES {
        clear();
        bytes.set(0);
    }
    bytes.set(bytes.get() + cost);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_memo_is_bounded_by_its_bytes_and_its_count() {
        let bytes = std::cell::Cell::new(0);
        let mut cleared = 0;
        assert!(!admit(0, 4, &bytes, ENTRY_BYTES + 1, || cleared += 1), "too large to keep");
        assert_eq!((bytes.get(), cleared), (0, 0));
        for _ in 0..(BYTES / ENTRY_BYTES) {
            assert!(admit(0, usize::MAX, &bytes, ENTRY_BYTES, || cleared += 1));
        }
        assert_eq!((bytes.get(), cleared), (BYTES, 0));
        assert!(admit(0, usize::MAX, &bytes, 1, || cleared += 1));
        assert_eq!((bytes.get(), cleared), (1, 1), "past the bytes: cleared first");
        assert!(admit(4, 4, &bytes, 1, || cleared += 1));
        assert_eq!((bytes.get(), cleared), (1, 2), "at the count: cleared first");
    }
}
