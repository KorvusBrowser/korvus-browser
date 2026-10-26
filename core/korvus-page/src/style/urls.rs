//! `url()` values and the base they resolve against.
//!
//! A relative URL in a stylesheet is relative to THE SHEET, not to the document: `url(rel.png)` in
//! `/sub/linked.css` is `/sub/rel.png`. Measured on `/usr/bin/google-chrome` 150.0.7871.46 -- the
//! request goes out for `/sub/rel.png`, and `getComputedStyle(el).backgroundImage` answers
//! `url("http://127.0.0.1:…/sub/rel.png")`, already absolute. The cascade is the last place that
//! knows which sheet a winning declaration came from, so that is where a value from a sheet with a
//! known URL is made absolute. A sheet written in the document has no URL of its own
//! here; its values stay as written and resolve against the document's base where they are used.
//!
//! A `url()` that reaches a property through a CUSTOM PROPERTY resolves against the sheet holding
//! the declaration that REFERENCES it, not the one that declared the property -- so absolutizing
//! happens a second time, after substitution, in [`crate::style::compute`]. Measured on stock Chrome
//! 150.0.7871.46: `:root{--shared:url(sharedimg.png)}` in `/sub/v.css` referenced by
//! `.cross{background-image:var(--shared)}` in `/other/cross.css` computes `/other/sharedimg.png`;
//! the same property declared in a `<style>` and referenced from `/sub/v.css` computes
//! `/sub/docshared.png`; referenced from a `style` attribute it computes `/sharedimg.png`. The
//! custom property's own value stays raw -- `getComputedStyle(el).getPropertyValue('--shared')` is
//! `url(sharedimg.png)`, relative -- which is why this cannot be done at declaration time.
//!
//! This crate carries no URL library (it is pure style and layout), so the resolver below is the
//! RFC 3986 §5.2 merge for the forms a stylesheet writes: scheme-relative, absolute-path, relative
//! path with dot segments, query-only and fragment-only.

/// Resolve `reference` against the absolute URL `base`.
pub fn resolve(base: &str, reference: &str) -> String {
    let r = reference.trim();
    if r.is_empty() {
        return base.to_string();
    }
    // Already absolute: a scheme is letters/digits/+-. followed by `:` before any `/`, `?` or `#`.
    if let Some(colon) = r.find(':') {
        let scheme = &r[..colon];
        if !scheme.is_empty()
            && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        {
            return r.to_string();
        }
    }
    let Some(scheme_end) = base.find("://") else { return r.to_string() };
    let scheme = &base[..scheme_end];
    let after = &base[scheme_end + 3..];
    let authority_end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let authority = &after[..authority_end];
    let rest = &after[authority_end..];
    let path_end = rest.find(['?', '#']).unwrap_or(rest.len());
    let base_path = if rest[..path_end].is_empty() { "/" } else { &rest[..path_end] };
    let base_query_end = rest.find('#').unwrap_or(rest.len());
    let base_query = &rest[path_end..base_query_end];

    if let Some(net) = r.strip_prefix("//") {
        return format!("{scheme}://{net}");
    }
    if r.starts_with('#') {
        return format!("{scheme}://{authority}{base_path}{base_query}{r}");
    }
    if r.starts_with('?') {
        return format!("{scheme}://{authority}{base_path}{r}");
    }
    let (ref_path, suffix) = match r.find(['?', '#']) {
        Some(i) => (&r[..i], &r[i..]),
        None => (r, ""),
    };
    let merged = if ref_path.starts_with('/') {
        ref_path.to_string()
    } else {
        let dir = &base_path[..base_path.rfind('/').map_or(0, |i| i + 1)];
        format!("{dir}{ref_path}")
    };
    format!("{scheme}://{authority}{}{suffix}", remove_dot_segments(&merged))
}

/// RFC 3986 §5.2.4.
fn remove_dot_segments(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let segments: Vec<&str> = path.split('/').collect();
    let last = segments.len().saturating_sub(1);
    for (i, seg) in segments.iter().enumerate() {
        match *seg {
            "." => {
                if i == last {
                    out.push("");
                }
            }
            ".." => {
                if out.len() > 1 {
                    out.pop();
                }
                if i == last {
                    out.push("");
                }
            }
            s => out.push(s),
        }
    }
    let joined = out.join("/");
    if joined.starts_with('/') {
        joined
    } else {
        format!("/{joined}")
    }
}

/// Every `url(…)` in a CSS value, unquoted, in order. `image-set(url(a) 1x, url(b) 2x)` answers both;
/// the caller decides which one a candidate list selects.
pub fn urls_in(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lower = value.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find("url(") {
        let Some((inner, next)) = url_at(value, from + i + 4) else { break };
        out.push(inner);
        from = next;
    }
    out
}

/// The reference of the `url(` whose argument starts at byte `start` (just past `url(`), unquoted and unescaped, and
/// the byte just past its `)`. A QUOTED reference runs to its closing quote and may hold a `)`: Framer's
/// `url('data:image/svg+xml,<g transform=%22translate(1 1)%22>…')` was cut at `translate(1 1`, so the computed value
/// was no URL Chrome's is and minea.com's logo mark never drew.
pub(crate) fn url_at(value: &str, start: usize) -> Option<(String, usize)> {
    let rest = &value[start..];
    let body = rest.trim_start();
    let lead = rest.len() - body.len();
    match body.chars().next() {
        Some(q @ ('"' | '\'')) => {
            let mut out = String::new();
            let mut escaped = false;
            for (i, c) in body[1..].char_indices() {
                if escaped {
                    out.push(c);
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == q {
                    let after = start + lead + 1 + i + c.len_utf8();
                    let close = value[after..].find(')')?;
                    return Some((out, after + close + 1));
                } else {
                    out.push(c);
                }
            }
            None
        }
        _ => {
            let end = rest.find(')')?;
            Some((rest[..end].trim().to_string(), start + end + 1))
        }
    }
}

/// `value` with every relative `url(…)` resolved against `base`, written the way Chrome serializes a
/// computed URL (`url("…")`), or `None` when the value holds no `url(`.
pub fn absolutize(value: &str, base: &str) -> Option<String> {
    let lower = value.to_ascii_lowercase();
    lower.find("url(")?;
    let mut out = String::with_capacity(value.len() + base.len());
    let mut from = 0;
    while let Some(i) = lower[from..].find("url(") {
        let start = from + i + 4;
        out.push_str(&value[from..start]);
        let Some((inner, next)) = url_at(value, start) else {
            out.push_str(&value[start..]);
            return Some(out);
        };
        // A fragment-only reference is LOCAL and keeps its spelling (Blink's `CSSUrlData::MakeComputed`): Chrome
        // 150 reads `fill: url(#a)` back as `url("#a")`, never the document's URL.
        let url = if inner.starts_with('#') { inner } else { resolve(base, &inner) };
        out.push('"');
        out.push_str(&url.replace('\\', "\\\\").replace('"', "\\\""));
        out.push('"');
        out.push(')');
        from = next;
    }
    out.push_str(&value[from..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // a quoted reference may hold a `)`; Chrome 152 keeps it whole
    #[test]
    fn a_quoted_url_runs_to_its_quote() {
        let v = "url('data:image/svg+xml,<g transform=%22translate(1 1)%22></g>') no-repeat";
        assert_eq!(urls_in(v), vec!["data:image/svg+xml,<g transform=%22translate(1 1)%22></g>"]);
        assert_eq!(
            absolutize(v, "http://h/s/a.css").as_deref(),
            Some("url(\"data:image/svg+xml,<g transform=%22translate(1 1)%22></g>\") no-repeat")
        );
    }

    #[test]
    fn a_reference_resolves_the_way_a_browser_resolves_it() {
        let base = "http://127.0.0.1:8000/sub/linked.css?v=1";
        assert_eq!(resolve(base, "rel.png"), "http://127.0.0.1:8000/sub/rel.png");
        assert_eq!(resolve(base, "../up.png"), "http://127.0.0.1:8000/up.png");
        assert_eq!(resolve(base, "/abs.png"), "http://127.0.0.1:8000/abs.png");
        assert_eq!(resolve(base, "//cdn.example/x.png"), "http://cdn.example/x.png");
        assert_eq!(resolve(base, "https://o.example/y"), "https://o.example/y");
        assert_eq!(resolve(base, "data:image/png;base64,AA"), "data:image/png;base64,AA");
        assert_eq!(resolve(base, "a/./b/../c.png?q#f"), "http://127.0.0.1:8000/sub/a/c.png?q#f");
        assert_eq!(resolve("http://h", "x.png"), "http://h/x.png");
    }

    #[test]
    fn urls_are_found_and_absolutized_in_place() {
        assert_eq!(urls_in("url(a.png), url(\"b.png\") no-repeat"), vec!["a.png", "b.png"]);
        assert_eq!(
            absolutize("url(a.png) no-repeat", "http://h/s/x.css").as_deref(),
            Some("url(\"http://h/s/a.png\") no-repeat")
        );
        assert_eq!(absolutize("none", "http://h/"), None);
    }
}
