//! The tags whose interface is plain `HTMLElement` come from `korvus_dom::tags::HTML_TAGS`, which
//! `tools/sync/gen_element_tags.py` emits from the vendored Blink tag table
//! (`gen/blink/core/html/html_tag_names.json5`); the gen crate's `webapi_gen::html_tags::HTML_TAGS` is
//! the same table emitted for the generator (W9.1 makes it a re-export). Once a hand
//! list in core/html/html_element.rs carried the 52 and this test re-derived it; the list is gone --
//! `install/dom_chain.rs::fill_dom_protos` fills the tag -> prototype map from the table -- so the
//! two emitted tables are asserted to agree on which tags are plain `HTMLElement`.
//!
//! Chrome on this machine (2026-09-09) agrees with that table for all 140 declared HTML tags -- the
//! probe was `document.createElement(t).constructor.name` for every one of them, and every answer
//! matched `HTML_TAGS`, `plaintext` -> `HTMLElement` included.

use std::collections::BTreeSet;

#[test]
fn the_plain_html_element_tag_set_is_blinks_own() {
    let derived: BTreeSet<&str> = webapi_gen::html_tags::HTML_TAGS
        .iter()
        .filter(|(_, iface)| *iface == "HTMLElement")
        .map(|(tag, _)| *tag)
        .collect();
    let shipped: BTreeSet<&str> = korvus_dom::tags::HTML_TAGS
        .iter()
        .filter(|(_, iface)| *iface == "HTMLElement")
        .map(|(tag, _)| *tag)
        .collect();
    let missing: Vec<&&str> = derived.difference(&shipped).collect();
    let extra: Vec<&&str> = shipped.difference(&derived).collect();
    assert!(missing.is_empty() && extra.is_empty(), "korvus_dom::tags drifted from Blink's table: missing {missing:?}, extra {extra:?}");
    assert_eq!(shipped.len(), 52, "Blink's plain-HTMLElement tag set");
}
