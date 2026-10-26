//! What real stylesheets lose on the way through this parser.
//!
//! P1's gate says to measure real sites once `getComputedStyle` works, because a scope decision
//! made against synthetic CSS is a guess. This is that measurement, kept in-tree so it can be
//! re-run rather than remembered.
//!
//! ```text
//! # fetch some real sheets first (any directory of .css files will do)
//! KORVUS_CSS_CORPUS=/tmp/realcss/sheets \
//!     cargo test -p korvus-page --release --test real_css -- --ignored --nocapture
//! ```
//!
//! ## The result, on 1.59 MB from MDN, GitHub, Wikipedia, and Hacker News
//!
//! ```text
//! declarations 33781/33968 kept (99.4%)
//! using var()   9757/9796  kept (99.6%)
//! selectors      770/772   parsed (99.7%)
//! ```
//!
//! Parsing is not the problem. **Computing is**: 9,796 declarations -- 28.8% of every declaration
//! in the corpus -- are written `color: var(--color-text-primary)`, and without substitution their
//! computed value is that string rather than an `rgb(...)`. On MDN and GitHub that is most of the
//! page's colour. See the Ruling.
//!
//! What is dropped outright is a long tail with no single cause: `content`, `mask-image`,
//! `@font-face`'s `src`/`unicode-range`/`font-display`, the logical-property shorthands
//! (`padding-inline`, `margin-inline`), and `color-scheme`. 187 declarations in 33,968.
use korvus_dom::Selector;
use page::style::parse::parse_stylesheet;
use page::style::rules::Origin;
use std::collections::BTreeMap;

fn corpus() -> Option<Vec<std::path::PathBuf>> {
    let dir = std::env::var("KORVUS_CSS_CORPUS").ok().filter(|d| !d.is_empty())?;
    let mut files: Vec<_> = std::fs::read_dir(&dir).ok()?.flatten().map(|e| e.path()).collect();
    files.sort();
    Some(files)
}

#[test]
#[ignore = "measurement; needs KORVUS_CSS_CORPUS pointing at a directory of real .css files"]
fn how_much_of_a_real_stylesheet_survives() {
    let Some(files) = corpus() else {
        println!("skip: set KORVUS_CSS_CORPUS to a directory of .css files");
        return;
    };
    let (mut bytes, mut total, mut kept) = (0usize, 0usize, 0usize);
    let (mut var_total, mut var_kept) = (0usize, 0usize);
    let mut lost: BTreeMap<String, usize> = BTreeMap::new();

    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else { continue };
        bytes += text.len();
        for (name, value) in declarations(&text) {
            total += 1;
            let uses_var = value.contains("var(");
            var_total += usize::from(uses_var);
            // Each declaration is parsed on its own, so "kept" means exactly that: the parser
            // produced something for it. A dropped one is a value the page asked for and does not
            // get.
            let one = parse_stylesheet(Origin::Author, &format!("x {{ {name}: {value} }}"));
            let survived = one.rules().first().is_some_and(|r| {
                !one.declarations_of(r).is_empty() || !one.raw_of(r).is_empty()
            });
            if survived {
                kept += 1;
                var_kept += usize::from(uses_var);
            } else {
                *lost.entry(name).or_default() += 1;
            }
        }
    }

    let pct = |a: usize, b: usize| if b == 0 { 0.0 } else { a as f64 * 100.0 / b as f64 };
    println!("files        {}", files.len());
    println!("bytes        {bytes}");
    println!("declarations {kept}/{total} kept ({:.1}%)", pct(kept, total));
    println!("using var()  {var_kept}/{var_total} kept ({:.1}%)", pct(var_kept, var_total));
    println!(
        "             {var_total} of {total} declarations ({:.1}%) are written with var(), and \
         without substitution every one of them computes to that string",
        pct(var_total, total)
    );
    let mut v: Vec<_> = lost.into_iter().collect();
    v.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    println!("\nmost-dropped properties ({} declarations):", v.iter().map(|(_, n)| n).sum::<usize>());
    for (name, n) in v.iter().take(20) {
        println!("  {n:6}  {name}");
    }
}

#[test]
#[ignore = "measurement; needs KORVUS_CSS_CORPUS pointing at a directory of real .css files"]
fn how_many_real_selectors_parse() {
    let Some(files) = corpus() else {
        println!("skip: set KORVUS_CSS_CORPUS to a directory of .css files");
        return;
    };
    let (mut total, mut ok) = (0usize, 0usize);
    let mut at_rules: BTreeMap<String, usize> = BTreeMap::new();
    let mut failed: Vec<String> = Vec::new();

    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else { continue };
        for prelude in preludes(&text) {
            let p = prelude.trim();
            if p.is_empty() {
                continue;
            }
            if let Some(rest) = p.strip_prefix('@') {
                let name = rest.split(|c: char| c.is_whitespace() || c == '(').next().unwrap_or("");
                *at_rules.entry(format!("@{name}")).or_default() += 1;
                continue;
            }
            total += 1;
            if Selector::parse_list(p).is_ok() {
                ok += 1;
            } else if failed.len() < 10 {
                failed.push(p.chars().take(90).collect());
            }
        }
    }
    println!("selectors {ok}/{total} parsed ({:.1}%)", ok as f64 * 100.0 / total.max(1) as f64);
    for f in &failed {
        println!("  did not parse: {f}");
    }
    println!("\nat-rules in the corpus (v1 understands only @media):");
    let mut a: Vec<_> = at_rules.into_iter().collect();
    a.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    for (name, n) in a.iter().take(12) {
        println!("  {n:6}  {name}");
    }
}

/// Every `name: value` in a declaration position.
fn declarations(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let b = text.as_bytes();
    let (mut i, mut depth) = (0usize, 0i32);
    while i < b.len() {
        match b[i] {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            b':' if depth > 0 => {
                let mut start = i;
                while start > 0 && (b[start - 1].is_ascii_alphanumeric() || b[start - 1] == b'-') {
                    start -= 1;
                }
                let name = &text[start..i];
                let is_property = !name.is_empty()
                    && (name.starts_with("--")
                        || name.starts_with(|c: char| c.is_ascii_alphabetic()));
                if is_property {
                    let mut j = i + 1;
                    let mut d = 0i32;
                    while j < b.len() {
                        match b[j] {
                            b'(' => d += 1,
                            b')' => d -= 1,
                            b';' | b'}' if d == 0 => break,
                            _ => {}
                        }
                        j += 1;
                    }
                    out.push((name.to_ascii_lowercase(), text[i + 1..j].trim().to_string()));
                    i = j;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// Every `{`-preceded prelude, at any depth.
fn preludes(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let b = text.as_bytes();
    let mut start = 0usize;
    for i in 0..b.len() {
        match b[i] {
            b'{' => {
                out.push(text[start..i].trim().to_string());
                start = i + 1;
            }
            b'}' | b';' => start = i + 1,
            _ => {}
        }
    }
    out
}
