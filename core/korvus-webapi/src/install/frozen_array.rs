//! THE `FrozenArray<T>` attribute, as Blink 150's bindings hand it out (C2, 2026-10-11).
//!
//! # The defect this closes
//!
//! Fingerprint Pro classified korvus `bad / browser_automation_studio / tampering / anti_detect_browser` on ONE
//! payload field: whether `navigator.languages`' `length` or any entry is writable or configurable. Chrome's is a
//! FROZEN array, the same object on every read; an anti-detect browser's JS override (`get: () => [...]`) is a
//! plain one, new every read -- and so was korvus's (`core/navigator/navigator.rs` built a `v8::Array` per read).
//! Splicing that one field both ways flipped the verdict 3/3 against its sham. The rule is WebIDL's and general:
//! a sweep of every FrozenArray attribute a page reaches without a permission or a device found korvus handing out
//! unfrozen arrays for `navigator.userAgentData.brands`, `DataTransfer.types`, `MediaMetadata.artwork` and
//! `.chapterInfo`, `IntersectionObserver.thresholds`, `CookieChangeEvent.changed` / `.deleted` and
//! `ElementInternals`' element lists, a new array per read where Chrome keeps one (`PerformanceObserver.
//! supportedEntryTypes`, `DataTransfer.types`) and the same one where Chrome makes a new one
//! (`FontFaceSetLoadEvent.fontfaces`, `ChapterInformation.artwork`), and no `ClipboardItem.types` at all
//! (`core/korvus-runtime/tests/frozen_arrays_live.rs`).
//!
//! # What Blink does, and therefore what this does
//!
//! A FrozenArray attribute's getter converts what the C++ returns with `ToV8Traits<IDLArray<T>>`: each entry as
//! `T` converts, into a JS Array, then `SetIntegrityLevel(kFrozen)` (bindings/core/v8/to_v8_traits.h,
//! platform/bindings/frozen_array_base.cc). What a SECOND read answers is the bindings' extended attribute when the
//! member has one, and otherwise the C++ getter's return type (`bind_gen/interface.py`'s
//! `make_return_value_cache_return_early`):
//!
//! ```text
//!   [SaveSameObject]          the first read's array, kept on the receiver                  Cache::Same
//!   [CachedAttribute=Pred]    the receiver's array until Pred() says the list changed       Cache::Cached
//!   a getter returning Blink's FrozenArray<T>: ONE array the platform object holds        Cache::Same
//!   a Vector / HeapVector / LocalVector, by value or by reference: a new array per read    Cache::Fresh
//! ```
//!
//! `[SameObject]` alone decides nothing -- Blink's bindings do not act on it, and `FontFaceSetLoadEvent.fontfaces`
//! (`[SameObject]`, returned BY VALUE) is a new array per read in Chrome 150. The table of every such attribute and
//! its rule is Blink's own, read from the IDL and the headers by `tools/sync/extract_blink_idl.py` into
//! `gen/blink/idl/blink-frozen-arrays.json`; nothing here names an interface.
//!
//! # Who calls it
//!
//! [`answer`] is the binding step, and every producer of a FrozenArray attribute's value goes through it or
//! through [`frozen`]: the D2 stub (`install::stub`, an empty list), the slot kits (`install::kits::evt_get` and
//! its siblings pick the fresh-copy or the stored-array getter by the row's rule), the constructor paths that
//! store a converted sequence (`install::ctor_dicts::is_frozen_attr`), and the hand-written bodies, which supply
//! only what Blink's C++ supplies -- the list, and for a `[CachedAttribute]` the version its predicate compares.

use deno_core::v8;
use std::collections::HashMap;
use std::sync::OnceLock;

/// What a second read of a FrozenArray attribute answers (the module doc's table).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Cache {
    /// A new frozen array every read.
    Fresh,
    /// The first read's array for as long as the receiver lives.
    Same,
    /// The receiver's array while the platform object reports the version it was made from.
    Cached,
}

/// One FrozenArray attribute of Blink's: its rule, and the privates its receiver keeps the array (and, for a
/// `[CachedAttribute]`, the version) under.
pub(crate) struct Row {
    pub(crate) cache: Cache,
    key: String,
    version_key: String,
}

/// `interface -> attribute -> row`, from Blink 150's IDL and headers (`tools/sync/extract_blink_idl.py`).
fn table() -> &'static HashMap<String, HashMap<String, Row>> {
    static TABLE: OnceLock<HashMap<String, HashMap<String, Row>>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let src: serde_json::Value = serde_json::from_str(include_str!("../../gen/blink/idl/blink-frozen-arrays.json")).unwrap_or_default();
        let mut out: HashMap<String, HashMap<String, Row>> = HashMap::new();
        for (name, e) in src.as_object().into_iter().flatten() {
            let Some((iface, attr)) = name.split_once('.') else { continue };
            let cache = match e.get("cache").and_then(|c| c.as_str()) {
                Some("same") => Cache::Same,
                Some("cached") => Cache::Cached,
                _ => Cache::Fresh,
            };
            let key = format!("korvus.frozenArray.{name}");
            let version_key = format!("{key}.version");
            out.entry(iface.to_string()).or_default().insert(attr.to_string(), Row { cache, key, version_key });
        }
        out
    })
}

/// Blink's row for `iface.attr`, or `None` when Blink declares no such FrozenArray attribute. A table row asks by
/// its label's interface: an `alias` table's label names its own (`Navigator::languages` serves `WorkerNavigator`
/// too, as Blink's `NavigatorLanguage` mixin does).
pub(crate) fn row(iface: &str, attr: &str) -> Option<&'static Row> {
    table().get(iface)?.get(attr)
}

/// `items` as a FrozenArray: a new JS Array of them, frozen (`Object.freeze`'s integrity level).
pub(crate) fn frozen<'s>(scope: &mut v8::PinScope<'s, '_>, items: &[v8::Local<'s, v8::Value>]) -> v8::Local<'s, v8::Array> {
    let a = v8::Array::new_with_elements(scope, items);
    let _ = a.set_integrity_level(scope, v8::IntegrityLevel::Frozen);
    a
}

/// The value of `iface.attr` read off `recv`, as Chrome 150's bindings answer it: the array the rule keeps for
/// `recv`, or else `items()` -- the platform object's list, its entries already converted as `T` converts --
/// frozen and, unless the rule is [`Cache::Fresh`], kept. `version` is asked only for a [`Cache::Cached`] row: a
/// number that changes whenever Blink's dirty predicate would say the list changed (a generation the object
/// bumps on each change, or a digest of the list). `None` when `items()` threw (the exception is pending).
///
/// A name Blink does not declare is answered fresh: the frozen array is WebIDL's whatever the row says, and only
/// the reuse needs Blink's table.
pub(crate) fn answer<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    recv: v8::Local<v8::Object>,
    iface: &str,
    attr: &str,
    version: impl FnOnce(&mut v8::PinScope<'s, '_>) -> f64,
    items: impl FnOnce(&mut v8::PinScope<'s, '_>) -> Option<Vec<v8::Local<'s, v8::Value>>>,
) -> Option<v8::Local<'s, v8::Value>> {
    let Some(r) = row(iface, attr).filter(|r| r.cache != Cache::Fresh) else {
        let list = items(scope)?;
        return Some(frozen(scope, &list).into());
    };
    let key = crate::binding::protos::private_key(scope, &r.key);
    // [CachedAttribute]: the predicate first, as the bindings ask it before they look at the cache.
    let now = (r.cache == Cache::Cached).then(|| version(scope));
    let kept = recv.get_private(scope, key).filter(|v| v.is_array());
    if let Some(kept) = kept {
        let current = match now {
            None => true,
            Some(n) => {
                let vk = crate::binding::protos::private_key(scope, &r.version_key);
                recv.get_private(scope, vk).and_then(|v| v.number_value(scope)).is_some_and(|was| was == n)
            }
        };
        if current {
            return Some(kept);
        }
    }
    let list = items(scope)?;
    let a: v8::Local<v8::Value> = frozen(scope, &list).into();
    recv.set_private(scope, key, a);
    if let Some(n) = now {
        let vk = crate::binding::protos::private_key(scope, &r.version_key);
        let nv = v8::Number::new(scope, n);
        recv.set_private(scope, vk, nv.into());
    }
    Some(a)
}

/// A digest of a list of strings as a [`answer`] version: equal lists, equal versions (FNV-1a, folded into the 53
/// bits a JS number holds exactly). For a `[CachedAttribute]` whose list is computed rather than stored, so that the
/// cache holds while the list is unchanged -- `navigator.languages`' `IsLanguagesDirty`.
pub(crate) fn digest<S: AsRef<str>>(list: &[S]) -> f64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for s in list {
        for b in s.as_ref().bytes().chain(std::iter::once(0xff)) {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    (h & ((1u64 << 53) - 1)) as f64
}

#[cfg(test)]
mod tests {
    use super::{digest, row, Cache};

    /// The rule of every attribute korvus answers through [`super::answer`] or the slot kits, read from Blink's
    /// table and asserted as Chrome 150 answered it (`fixtures/frozen-arrays/`, three runs alike): the reuse a
    /// second read shows.
    #[test]
    fn the_rules_are_blinks() {
        for (iface, attr, want) in [
            ("Navigator", "languages", Cache::Cached),
            ("WorkerNavigator", "languages", Cache::Cached),
            ("NavigatorUAData", "brands", Cache::Fresh),
            ("DataTransfer", "types", Cache::Cached),
            ("MessageEvent", "ports", Cache::Cached),
            ("ClipboardItem", "types", Cache::Fresh),
            ("MediaMetadata", "artwork", Cache::Fresh),
            ("MediaMetadata", "chapterInfo", Cache::Fresh),
            ("ChapterInformation", "artwork", Cache::Fresh),
            ("IntersectionObserver", "thresholds", Cache::Fresh),
            // [SameObject] and returned by value: a new array per read
            ("FontFaceSetLoadEvent", "fontfaces", Cache::Fresh),
            ("CookieChangeEvent", "changed", Cache::Same),
            ("RTCTrackEvent", "streams", Cache::Same),
            ("Notification", "vibrate", Cache::Same),
            ("PerformanceObserver", "supportedEntryTypes", Cache::Same),
            // no extended attribute: the C++ holds one FrozenArray
            ("ResizeObserverEntry", "contentBoxSize", Cache::Same),
            ("CSSContainerRule", "conditions", Cache::Same),
            ("ElementInternals", "ariaControlsElements", Cache::Same),
            ("Element", "ariaLabelledByElements", Cache::Same),
            ("PerformanceResourceTiming", "serverTiming", Cache::Fresh),
            ("USBIsochronousInTransferResult", "packets", Cache::Fresh),
        ] {
            assert_eq!(row(iface, attr).map(|r| r.cache), Some(want), "{iface}.{attr}");
        }
        // an attribute Blink declares as something else is not a row
        assert!(row("Navigator", "language").is_none());
        assert!(row("Event", "composedPath").is_none());
    }

    #[test]
    fn a_digest_tells_lists_apart_and_is_an_exact_js_number() {
        let a = digest(&["en-US", "en"]);
        assert_eq!(a, digest(&["en-US".to_string(), "en".to_string()]));
        assert_ne!(a, digest(&["en", "en-US"]));
        assert_ne!(a, digest(&["en-USen"]));
        assert_ne!(digest::<&str>(&[]), digest(&[""]));
        assert!(a < 9_007_199_254_740_992.0 && a.fract() == 0.0);
    }
}
