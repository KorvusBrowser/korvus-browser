//! `core/fileapi/opfs_timing.rs` -- how long the browser's storage service takes to answer an origin private file
//! system call, and `navigator.storage.estimate()`: the delay before each promise settles.
//!
//! Chrome 150.0.7871.46 measured (a cross-site frame, a fresh profile, three runs alike; every
//! call awaited before the next, `performance.now()` around each):
//!
//! | what                                                                  | Chrome 150        |
//! |-----------------------------------------------------------------------|-------------------|
//! | a storage key's FIRST `getDirectory()` -- the first key of the session | 4.8-10.2 ms       |
//! | ... a later key's                                                     | 3.4-3.9 ms        |
//! | `getDirectory()` again, the page's or a worker's                      | 0.0-0.3 ms        |
//! | the FIRST operation after the bucket opened (`estimate()`, a file's   | 6.2-6.6 ms (first |
//! | create or lookup, ...) -- once per key                                | key), 3.5-5.2 ms  |
//! | every other operation (handles, `getFile`, `keys()`, `remove`, ...)   | 0.0-0.4 ms        |
//! | `estimate()` of a key with no bucket                                  | 0.3-0.5 ms        |
//!
//! korvus answered every operation a 1 ms task later and every `getDirectory()` 20 ms later: a challenge provider's
//! challenge times its OPFS step (`getDirectory()` in the frame, then a worker's `getFileHandle(create)`,
//! `createSyncAccessHandle()`, write and flush) and posts it; Chrome's step took 20 ms, korvus's 69.
//!
//! One service answers in order: an operation issued while an earlier one is still being answered waits for it
//! (`busy_until`), so a later call never settles before an earlier one. The state is the process's -- the
//! page's realms and its workers share one storage service, as Chrome's renderers share the browser's.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// The first storage key the session opens.
const OPEN_FIRST_MS: f64 = 6.0;
/// A later storage key's first `getDirectory()`.
const OPEN_LATER_MS: f64 = 3.7;
/// The first operation after a key's bucket opened: the first key's, then a later key's.
const BACKEND_FIRST_MS: f64 = 6.4;
const BACKEND_LATER_MS: f64 = 3.7;
/// Any other answer.
const ANSWER_MS: f64 = 0.15;
/// `estimate()` of a key no `getDirectory()` has opened.
const ESTIMATE_UNOPENED_MS: f64 = 0.4;

/// What the call asks the storage service.
#[derive(Clone, Copy)]
pub(crate) enum Op {
    /// `navigator.storage.getDirectory()`.
    OpenRoot,
    /// Any operation on a handle or stream of the key's bucket.
    Backend,
    /// `navigator.storage.estimate()`.
    Estimate,
}

#[derive(Default)]
struct Key {
    /// The bucket has been asked for (`getDirectory()`).
    opened: bool,
    /// The first operation after the bucket opened has been asked.
    backend: bool,
    /// When the service is done with the key's queued answers (a [`spec::process_ms`] value).
    busy_until: f64,
}

#[derive(Default)]
struct Service {
    first_key: Option<String>,
    keys: HashMap<String, Key>,
}

fn service() -> std::sync::MutexGuard<'static, Service> {
    static S: OnceLock<Mutex<Service>> = OnceLock::new();
    S.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// The milliseconds from now until the storage service answers `op` for `key` (an origin).
pub(crate) fn answer_ms(key: &str, op: Op) -> f64 {
    answer_at(key, op, spec::process_ms(std::time::Instant::now()))
}

/// [`answer_ms`] asked at `now` (a [`spec::process_ms`] value).
fn answer_at(key: &str, op: Op, now: f64) -> f64 {
    let mut s = service();
    let first = s.first_key.get_or_insert_with(|| key.to_string()) == key;
    let k = s.keys.entry(key.to_string()).or_default();
    let cost = match op {
        Op::OpenRoot if !k.opened => {
            k.opened = true;
            if first { OPEN_FIRST_MS } else { OPEN_LATER_MS }
        }
        Op::Estimate if !k.opened => ESTIMATE_UNOPENED_MS,
        Op::Backend | Op::Estimate if !k.backend => {
            k.backend = true;
            if first { BACKEND_FIRST_MS } else { BACKEND_LATER_MS }
        }
        _ => ANSWER_MS,
    };
    let done = now.max(k.busy_until) + cost;
    k.busy_until = done;
    done - now
}

#[cfg(test)]
mod tests {
    use super::*;

    // The service is the process's and the tests share it, so no test assumes it opened the session's first key. Each
    // call is asked at a stated time: the answer is a delay from that time, so two calls read the clock apart compared
    // two delays from two moments (the reopen check failed whenever the calls were more than 0.15 ms apart).
    #[test]
    fn a_key_pays_to_open_then_once_more_to_start_and_answers_keep_their_order() {
        let key = "https://opfs-timing-a.example";
        let t = spec::process_ms(std::time::Instant::now());
        let open = t + answer_at(key, Op::OpenRoot, t);
        assert!((OPEN_LATER_MS - 0.5..OPEN_FIRST_MS + 0.5).contains(&(open - t)), "{}", open - t);
        let start = t + answer_at(key, Op::Backend, t);
        assert!(start >= open + BACKEND_LATER_MS - 0.5, "the first operation waits for the opening, then starts: {start} after {open}");
        let next = t + 0.05 + answer_at(key, Op::Backend, t + 0.05);
        assert!(next >= start, "answers keep their order: {next} {start}");
        let reopen = t + 0.1 + answer_at(key, Op::OpenRoot, t + 0.1);
        assert!(reopen >= next && reopen < next + 1.0, "an opened key's getDirectory is one more answer: {reopen} {next}");
    }

    #[test]
    fn a_key_with_no_bucket_estimates_quickly() {
        let est = answer_ms("https://opfs-timing-b.example", Op::Estimate);
        assert!((ESTIMATE_UNOPENED_MS - 0.1..ESTIMATE_UNOPENED_MS + 0.5).contains(&est), "{est}");
    }
}
