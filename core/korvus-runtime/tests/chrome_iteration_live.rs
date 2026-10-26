//! `forEach` and the iterators of WebIDL's iterable, maplike and setlike interfaces, as stock Chrome 150 runs them.
//! a bot challenge walks `URLSearchParams` and `KeyboardLayoutMap` with `forEach`;
//! the sweep its side-by-side prompted found every `forEach` this engine implements off Chrome's:
//!
//! * the argument count is checked before the callback's type, and the type is an IDL `Function` -- "parameter 1
//!   is not of type 'Function'." on all ten interfaces, where this engine said a callback interface's "The
//!   callback provided as parameter 1 is not a function." (seven) or checked nothing (`URLSearchParams`,
//!   `Headers`, `FormData`: no throw at all);
//! * the pair iterables called the callback with `this` undefined, ignoring `thisArg`, and kept walking after it
//!   threw;
//! * a pair iterable's `forEach` and iterators walk the list AS IT STANDS at each step, by index (WebIDL's
//!   default iterator): an entry deleted before the walk reaches it is not seen, one appended is, deleting the
//!   current entry skips the one after it (`Headers` indexes its re-sorted list), and an iterator that reached the
//!   end yields an entry appended afterwards. This engine iterated a snapshot.
//!
//! Still this engine's (not probed here): a maplike's or setlike's walk is a snapshot, where Chrome's follows the
//! container -- `HighlightRegistry` and `CustomStateSet` skip an entry deleted ahead of the walk, visit one added,
//! and `CustomStateSet`'s exhausted iterator yields a later one while `HighlightRegistry`'s stays done.
//!
//! The answers were recorded from `/usr/bin/google-chrome` 150.0.7871.46, three runs alike, from
//! `fixtures/iteration/for-each-probe.js` on the page `chrome_canvas_and_errors_live` serves.

use crate::chrome_canvas_and_errors_live::{answers, assert_as_chrome, Mode};

#[tokio::test]
async fn for_each_and_iterators_as_chrome_150_runs_them() {
    let got = answers(Mode::Page, include_str!("fixtures/iteration/for-each-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/iteration/for-each-chrome-152.json"));
}
