//! Does matching hold up on a page the size of a real one?
//!
//! The design calls the rule index "not an optimization" — the difference between styling a page
//! and hanging on one — and calls the bloom filter the same. The first claim is easy to believe and
//! easy to check; the second is the one worth measuring before paying for it, because the filter
//! costs a hash per element per ancestor and a second rejection pass on every candidate.
//!
//! Measured on this machine, 10,008 nodes x 3,000 rules, `--release`:
//!
//! ```text
//!                        index only     + ancestor bloom
//!   realistic sheet       115.7 ms          19.1 ms      (6.1x)
//!   same rightmost key    655.1 ms          82.0 ms      (8.0x)
//! ```
//!
//! The second row is why the filter is in: 3,000 rules, every one of them ending in `a`, and not
//! one of them matches anything. The index cannot tell them apart; the filter rejects them all
//! without a tree walk.
//!
//! Run it with the numbers printed:
//!
//! ```text
//! cargo test -p korvus-page --release --test matching_scale -- --ignored --nocapture
//! ```
use korvus_dom::Dom;
use page::style::matching::Matcher;
use page::style::media::MediaContext;
use page::style::parse::parse_stylesheet;
use page::style::rules::Origin;

/// A stylesheet shaped like a real one: mostly single-class rules, some descendant selectors, a
/// scattering of ids, and a handful of bare tag rules — not six hundred of them.
fn stylesheet(rules: usize) -> String {
    let mut css = String::new();
    for i in 0..rules {
        match i % 5 {
            0 => css.push_str(&format!(".c{i} {{ color: red; margin: {i}px }}\n")),
            1 => css.push_str(&format!(".wrap .c{i} {{ padding: {i}px }}\n")),
            2 => css.push_str(&format!("div.c{i} span {{ color: blue }}\n")),
            3 => css.push_str(&format!("#id{i} {{ width: {i}px }}\n")),
            _ => css.push_str(&format!("li.c{i} {{ color: teal }}\n")),
        }
    }
    css
}

/// The shape the rule index cannot help with: thousands of descendant selectors that all end in the
/// same element. Every `<a>` becomes a candidate for every one of them, and each candidate costs a
/// walk up the tree. This is the case `AncestorHashes` plus a bloom filter exists to reject.
fn same_rightmost_key(rules: usize) -> String {
    let mut css = String::new();
    for i in 0..rules {
        css.push_str(&format!(".section-{i} .row .cell a {{ color: red }}\n"));
    }
    css
}

/// A document shaped like a real one: a wrapper chain with a wide list under it.
fn document(nodes: usize) -> String {
    let mut html = String::from("<div class='wrap'><div class='page'><main><ul>");
    for i in 0..nodes {
        html.push_str(&format!(
            "<li id='id{i}' class='c{} item'><span class='c{}'><a href='#'>t</a></span></li>",
            i % 997,
            i % 503
        ));
    }
    html.push_str("</ul></main></div></div>");
    html
}

fn style_everything(css: &str, html: &str) -> (usize, usize, std::time::Duration) {
    let sheets = vec![parse_stylesheet(Origin::Author, css)];
    let matcher = Matcher::build(&sheets);
    let dom = Dom::parse(html);
    let media = MediaContext::default();
    let mut out = Vec::new();
    let mut matches = 0usize;

    let start = std::time::Instant::now();
    for node in 0..dom.len() as u32 {
        matcher.match_element(&sheets, &dom, node, &media, &mut out);
        matches += out.len();
    }
    (dom.len(), matches, start.elapsed())
}

fn report(label: &str, css: &str, html: &str) {
    let (nodes, matches, elapsed) = style_everything(css, html);
    println!(
        "{label}: {nodes} nodes -> {matches} matched rules in {elapsed:?} ({:.1} us/node)",
        elapsed.as_secs_f64() * 1e6 / nodes as f64
    );
}

// What a page actually looks like. This is the number that matters.
#[test]
#[ignore = "measurement, not a gate; run with --release --nocapture"]
fn a_realistic_page_styles_in_a_sane_amount_of_time() {
    report("realistic  ", &stylesheet(3000), &document(2500));
}

// The adversarial shape: every rule ends in `a`, so the index hands every `<a>` all 3,000 and each
// one costs an ancestor walk. If the bloom filter is worth its cost anywhere, it is here.
#[test]
#[ignore = "measurement, not a gate; run with --release --nocapture"]
fn thousands_of_descendant_selectors_sharing_one_rightmost_key() {
    report("same key   ", &same_rightmost_key(3000), &document(2500));
}
