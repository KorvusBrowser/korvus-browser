//! `core/html/srcset.rs` -- WHICH image an `<img>` (or the `<source>` its `<picture>` selected) asks
//! for: `srcset` candidate parsing, `sizes`, and the density selection.
//!
//! THE DEFECT. `subresource.rs` took the FIRST candidate of a `srcset` and fetched it as soon as the
//! attribute was written, and fetched `src` too when `src` was set before `srcset`. Measured against
//! Chrome 150.0.7871.46 over a loopback server that logs paths (headless and headed agree):
//!
//! ```text
//! <img src=hh-src sizes="(max-width: 479px) 85vw, ..., 32vw"
//!      srcset="hh-500 500w, hh-800 800w, ...">            korvus hh-500           chrome hh-800
//! img.src="ia-a"; img.srcset="ia-b 2x"  (one task)        korvus ia-a AND ia-b    chrome ia-a
//! <img src=sy-src srcset="sy-2x 2x">                      korvus sy-2x            chrome sy-src
//! <picture><source media="(max-width: 800px)" ...>        korvus the source       chrome the <img>
//! ```
//!
//! THE RULES, each measured on that Chrome at DPR 1 and -- in a probe of our own Chrome instance
//! started with `--force-device-scale-factor=2`, never on the oracle -- at DPR 2:
//!
//! * candidates are parsed with HTML's srcset algorithm (Blink `html_srcset_parser.cc`), a candidate
//!   with a bad descriptor is dropped, `src` joins as a 1x candidate unless a candidate has a `w`
//!   descriptor;
//! * a `w` candidate's density is its width over the `sizes` length, and `sizes` is the first entry
//!   whose media condition matches (no valid entry: `100vw`); `vw` is the WINDOW's inner width
//!   (1920 on a page whose `documentElement.clientWidth` is 1905 picks the same candidate as 1920);
//! * the candidates are stably sorted by density, equal densities keep the first, and the winner is
//!   the FIRST CANDIDATE WHOSE DENSITY IS AT LEAST THE DEVICE PIXEL RATIO, else the densest. That is
//!   Blink's `SrcsetSelectionMatchesImageSet` branch, and the older geometric-mean branch beside it
//!   in the same file is NOT what Chrome 150 runs: `srcset="1.5x, 4x"` at DPR 2 fetches the 4x
//!   (the geometric mean, 2.45, would have kept 1.5x).
//!
//! The device pixel ratio is the PERSONA's (`screen().device_pixel_ratio`, the same number
//! `window.devicePixelRatio` answers), read through [`crate::core::dom::geometry_queries::media_context`]
//! -- this file holds no ratio of its own.

use page::style::media::{MediaContext, MediaQueryList};
use page::value::ResolveContext;

/// One parsed candidate.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Candidate {
    pub url: String,
    /// `x` descriptor, when there was one.
    pub density: Option<f32>,
    /// `w` descriptor, when there was one.
    pub width: Option<u32>,
    /// This candidate is the element's `src`, not a `srcset` entry.
    pub from_src: bool,
}

fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\x0c' | '\r')
}

/// HTML's "parse a srcset attribute", as Blink implements it (`ParseImageCandidatesFromSrcsetAttribute`).
pub(crate) fn parse_srcset(attr: &str) -> Vec<Candidate> {
    let chars: Vec<char> = attr.chars().collect();
    let n = chars.len();
    let mut pos = 0;
    let mut out = Vec::new();
    while pos < n {
        while pos < n && (is_space(chars[pos]) || chars[pos] == ',') {
            pos += 1;
        }
        if pos >= n {
            break;
        }
        let url_start = pos;
        while pos < n && !is_space(chars[pos]) {
            pos += 1;
        }
        let mut url_end = pos;
        let mut descriptors: Vec<String> = Vec::new();
        if chars[pos - 1] == ',' {
            while url_end > url_start && chars[url_end - 1] == ',' {
                url_end -= 1;
            }
            if url_end == url_start {
                continue;
            }
        } else {
            while pos < n && is_space(chars[pos]) {
                pos += 1;
            }
            // Tokenize descriptors up to the next top-level comma.
            let mut cur = String::new();
            let mut in_paren = false;
            while pos < n {
                let c = chars[pos];
                if in_paren {
                    cur.push(c);
                    if c == ')' {
                        in_paren = false;
                    }
                    pos += 1;
                    continue;
                }
                if c == ',' {
                    pos += 1;
                    break;
                }
                if is_space(c) {
                    if !cur.is_empty() {
                        descriptors.push(std::mem::take(&mut cur));
                    }
                } else {
                    if c == '(' {
                        in_paren = true;
                    }
                    cur.push(c);
                }
                pos += 1;
            }
            if !cur.is_empty() {
                descriptors.push(cur);
            }
        }
        let url: String = chars[url_start..url_end].iter().collect();
        let mut c = Candidate { url, density: None, width: None, from_src: false };
        let mut height = false;
        let mut ok = true;
        for d in &descriptors {
            let (body, last) = d.split_at(d.len() - d.chars().last().map_or(0, char::len_utf8));
            match last {
                "w" => {
                    if c.density.is_some() || c.width.is_some() {
                        ok = false;
                        break;
                    }
                    match (!body.is_empty() && body.bytes().all(|b| b.is_ascii_digit())).then(|| body.parse::<u32>().ok()).flatten() {
                        Some(w) if w > 0 => c.width = Some(w),
                        _ => {
                            ok = false;
                            break;
                        }
                    }
                }
                "h" => {
                    if c.density.is_some() || height {
                        ok = false;
                        break;
                    }
                    match (!body.is_empty() && body.bytes().all(|b| b.is_ascii_digit())).then(|| body.parse::<u32>().ok()).flatten() {
                        Some(h) if h > 0 => height = true,
                        _ => {
                            ok = false;
                            break;
                        }
                    }
                }
                "x" => {
                    if c.density.is_some() || height || c.width.is_some() || body.starts_with('+') {
                        ok = false;
                        break;
                    }
                    match body.parse::<f32>().ok().filter(|v| v.is_finite() && *v >= 0.0) {
                        Some(v) => c.density = Some(v),
                        None => {
                            ok = false;
                            break;
                        }
                    }
                }
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        if ok && height && c.width.is_none() {
            ok = false;
        }
        if ok {
            out.push(c);
        }
    }
    out
}

/// Split on commas that are not inside parentheses.
fn split_top_level(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth <= 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

/// A `<length>` in CSS pixels, as the sizes attribute resolves it: font-relative units against the
/// initial font size (there is no element style to consult -- Blink's `MediaValues` does the same),
/// viewport units against the window.
fn length_px(value: f32, unit: &str, ctx: &MediaContext) -> Option<f32> {
    let rc = ResolveContext::default();
    let u = unit.to_ascii_lowercase();
    Some(value * match u.as_str() {
        "px" => 1.0,
        "in" => 96.0,
        "cm" => 96.0 / 2.54,
        "mm" => 96.0 / 25.4,
        "q" => 96.0 / 101.6,
        "pt" => 96.0 / 72.0,
        "pc" => 16.0,
        "em" => rc.font_size,
        "rem" => rc.root_font_size,
        "ex" => rc.x_height,
        "ch" => rc.zero_advance,
        "vw" => ctx.viewport_width / 100.0,
        "vh" => ctx.viewport_height / 100.0,
        "vmin" => ctx.viewport_width.min(ctx.viewport_height) / 100.0,
        "vmax" => ctx.viewport_width.max(ctx.viewport_height) / 100.0,
        _ => return None,
    })
}

/// A tiny evaluator for `calc()`/`min()`/`max()`/`clamp()` over lengths and numbers. Values carry a
/// flag: `true` = a length in px, `false` = a plain number.
struct MathParser<'a> {
    s: &'a [u8],
    i: usize,
    ctx: &'a MediaContext,
}

impl MathParser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && (self.s[self.i] as char).is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn sum(&mut self) -> Option<(f32, bool)> {
        let mut a = self.product()?;
        loop {
            self.ws();
            let Some(&c) = self.s.get(self.i) else { return Some(a) };
            if c != b'+' && c != b'-' {
                return Some(a);
            }
            self.i += 1;
            let b = self.product()?;
            if a.1 != b.1 {
                return None;
            }
            a.0 = if c == b'+' { a.0 + b.0 } else { a.0 - b.0 };
        }
    }
    fn product(&mut self) -> Option<(f32, bool)> {
        let mut a = self.atom()?;
        loop {
            self.ws();
            let Some(&c) = self.s.get(self.i) else { return Some(a) };
            if c != b'*' && c != b'/' {
                return Some(a);
            }
            self.i += 1;
            let b = self.atom()?;
            if c == b'*' {
                if a.1 && b.1 {
                    return None;
                }
                a = (a.0 * b.0, a.1 || b.1);
            } else {
                if b.1 || b.0 == 0.0 {
                    return None;
                }
                a.0 /= b.0;
            }
        }
    }
    fn args(&mut self) -> Option<Vec<(f32, bool)>> {
        let mut v = vec![self.sum()?];
        loop {
            self.ws();
            match self.s.get(self.i) {
                Some(b',') => {
                    self.i += 1;
                    v.push(self.sum()?);
                }
                Some(b')') => {
                    self.i += 1;
                    return Some(v);
                }
                _ => return None,
            }
        }
    }
    fn atom(&mut self) -> Option<(f32, bool)> {
        self.ws();
        let rest = std::str::from_utf8(&self.s[self.i..]).ok()?;
        let lower = rest.to_ascii_lowercase();
        for (name, kind) in [("calc(", 0), ("min(", 1), ("max(", 2), ("clamp(", 3), ("(", 0)] {
            if lower.starts_with(name) {
                self.i += name.len();
                let v = self.args()?;
                if v.iter().any(|x| x.1 != v[0].1) {
                    return None;
                }
                let is_len = v[0].1;
                let val = match (kind, v.len()) {
                    (0, 1) => v[0].0,
                    (1, _) => v.iter().map(|x| x.0).fold(f32::INFINITY, f32::min),
                    (2, _) => v.iter().map(|x| x.0).fold(f32::NEG_INFINITY, f32::max),
                    (3, 3) => v[1].0.min(v[2].0).max(v[0].0),
                    _ => return None,
                };
                return Some((val, is_len));
            }
        }
        // A number, optionally followed by a unit.
        let bytes = rest.as_bytes();
        let mut j = 0;
        if j < bytes.len() && (bytes[j] == b'+' || bytes[j] == b'-') {
            j += 1;
        }
        let num_start = 0;
        while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b'.') {
            j += 1;
        }
        let num: f32 = rest[num_start..j].parse().ok()?;
        let unit_start = j;
        while j < bytes.len() && bytes[j].is_ascii_alphabetic() {
            j += 1;
        }
        let unit = &rest[unit_start..j];
        self.i += j;
        if unit.is_empty() {
            return Some((num, false));
        }
        Some((length_px(num, unit, self.ctx)?, true))
    }
}

/// A `<source-size-value>`: a non-negative length, a unitless zero, or a math function of lengths.
fn source_size_value(text: &str, ctx: &MediaContext) -> Option<f32> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    let lower = t.to_ascii_lowercase();
    if ["calc(", "min(", "max(", "clamp("].iter().any(|p| lower.starts_with(p)) {
        let mut p = MathParser { s: t.as_bytes(), i: 0, ctx };
        let v = p.atom()?;
        p.ws();
        return (p.i == t.len() && v.1).then_some(v.0.max(0.0));
    }
    if t == "0" {
        return Some(0.0);
    }
    let split = t.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '+' || c == '-')).unwrap_or(t.len());
    let num: f32 = t[..split].parse().ok()?;
    let unit = &t[split..];
    if unit.is_empty() || !unit.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    let px = length_px(num, unit, ctx)?;
    (px >= 0.0).then_some(px)
}

/// What `sizes` resolves to, in CSS pixels (Blink `SizesAttributeParser`). `None` attribute or no
/// valid entry is `100vw`. `auto` is honoured only where Blink honours it (a rendered, lazy image
/// whose layout width is known) -- this engine does not size an `<img>` from its decoded bytes, so
/// it takes the same `100vw` fallback the parser does when that condition fails.
pub(crate) fn source_size(sizes: Option<&str>, ctx: &MediaContext) -> f32 {
    let fallback = ctx.viewport_width;
    let Some(sizes) = sizes else { return fallback };
    for entry in split_top_level(sizes) {
        let e = entry.trim();
        if e.is_empty() {
            continue;
        }
        if e.len() >= 4 && e[..4].eq_ignore_ascii_case("auto") && (e.len() == 4 || e[4..].starts_with(is_space)) {
            return fallback;
        }
        // The size value is the LAST component: either a function whose parenthesis closes the entry,
        // or the last whitespace-separated token.
        let (cond, value) = if e.ends_with(')') {
            let bytes = e.as_bytes();
            let mut depth = 0i32;
            let mut open = None;
            for k in (0..bytes.len()).rev() {
                match bytes[k] {
                    b')' => depth += 1,
                    b'(' => {
                        depth -= 1;
                        if depth == 0 {
                            open = Some(k);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let Some(open) = open else { continue };
            let name_start = e[..open].rfind(|c: char| !c.is_ascii_alphabetic()).map_or(0, |k| k + 1);
            let name = &e[name_start..open];
            if !["calc", "min", "max", "clamp"].iter().any(|f| f.eq_ignore_ascii_case(name)) {
                continue; // a bare media condition with no size after it
            }
            (&e[..name_start], &e[name_start..])
        } else {
            match e.rfind(is_space) {
                Some(k) => (&e[..k], &e[k + 1..]),
                None => ("", e),
            }
        };
        let cond = cond.trim();
        if !cond.is_empty() && !MediaQueryList::parse(cond, None).matches(ctx, &[]) {
            continue;
        }
        if let Some(px) = source_size_value(value, ctx) {
            return px;
        }
    }
    fallback
}

/// Blink's `PickBestImageCandidate` (`candidates` are the srcset entries followed by `src`, if any), with the winner's
/// pixel density -- a `w` candidate's width over the source size, an `x` one's own --
/// which the image's natural size is divided by (`naturalWidth` of a 20px image picked as `512w` for a 1280px
/// slot is 50, Chrome 150).
pub(crate) fn pick_density(mut candidates: Vec<Candidate>, dpr: f32, source_size: f32) -> Option<(Candidate, f32)> {
    if candidates.is_empty() {
        return None;
    }
    let mut ignore_src = false;
    let mut dens: Vec<(f32, usize)> = Vec::with_capacity(candidates.len());
    for (i, c) in candidates.iter().enumerate() {
        let d = if let Some(w) = c.width {
            ignore_src = true;
            w as f32 / source_size
        } else {
            c.density.unwrap_or(1.0)
        };
        dens.push((d, i));
    }
    dens.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut deduped: Vec<(f32, usize)> = Vec::new();
    let mut prev = -1.0f32;
    for (d, i) in dens {
        if d != prev && (!ignore_src || !candidates[i].from_src) {
            deduped.push((d, i));
        }
        prev = d;
    }
    if deduped.is_empty() {
        return None;
    }
    let mut winner = deduped.len() - 1;
    for (k, (d, _)) in deduped.iter().enumerate() {
        if *d >= dpr {
            winner = k;
            break;
        }
    }
    // A higher-density candidate that is a `data:` URL costs no request: Blink takes it.
    for k in (winner + 1..deduped.len()).rev() {
        if candidates[deduped[k].1].url.trim().get(..5).is_some_and(|p| p.eq_ignore_ascii_case("data:")) {
            winner = k;
            break;
        }
    }
    let wd = deduped[winner].0;
    while winner > 0 && deduped[winner - 1].0 == wd {
        winner -= 1;
    }
    let density = deduped[winner].0;
    Some((candidates.swap_remove(deduped[winner].1), density))
}

/// The URL an element with these attributes selects: `srcset` (+ `src` as a candidate), or plain
/// `src` when there is no `srcset` attribute at all. `None` = no request.
#[cfg(test)]
pub(crate) fn select(src: Option<&str>, srcset: Option<&str>, sizes: Option<&str>, ctx: &MediaContext) -> Option<String> {
    select_density(src, srcset, sizes, ctx).map(|(u, _)| u)
}

/// [`select`], with the selected candidate's pixel density ([`pick_density`]); 1 for a plain `src`.
pub(crate) fn select_density(src: Option<&str>, srcset: Option<&str>, sizes: Option<&str>, ctx: &MediaContext) -> Option<(String, f32)> {
    let Some(srcset) = srcset else {
        return src.map(|s| (s.to_string(), 1.0));
    };
    let mut c = parse_srcset(srcset);
    if let Some(s) = src.filter(|s| !s.is_empty()) {
        c.push(Candidate { url: s.to_string(), density: None, width: None, from_src: true });
    }
    let size = source_size(sizes, ctx);
    pick_density(c, ctx.dppx, size).map(|(c, d)| (c.url, d))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(dpr: f32) -> MediaContext {
        MediaContext { viewport_width: 1920.0, viewport_height: 993.0, dppx: dpr, ..MediaContext::default() }
    }

    /// The selected candidate's density, which the image's natural size is divided by: a `w` candidate's width over
    /// the source size, an `x` candidate's own, 1 for a plain `src` (Chrome 150: a 20px image picked as `512w` for a
    /// 1280px window reads naturalWidth 50; as `1024w` for `50vw`, 12).
    #[test]
    fn the_selected_candidate_carries_its_density() {
        let c = MediaContext { viewport_width: 1280.0, viewport_height: 800.0, dppx: 1.0, ..MediaContext::default() };
        assert_eq!(select_density(Some("w0"), Some("w1 512w"), None, &c), Some(("w1".to_string(), 0.4)));
        assert_eq!(select_density(None, Some("b1 512w, b2 1024w"), Some("50vw"), &c), Some(("b2".to_string(), 1.6)));
        assert_eq!(select_density(None, Some("x1 2x"), None, &c), Some(("x1".to_string(), 2.0)));
        assert_eq!(select_density(Some("s"), None, None, &c), Some(("s".to_string(), 1.0)));
    }

    /// Every row is a Chrome 150 measurement (headless + headed at DPR 1, a DPR-2 probe of our own).
    #[test]
    fn density_selection_matches_chrome() {
        let rows: &[(Option<&str>, &str, Option<&str>, &str, &str)] = &[
            (None, "sx-1x 1x, sx-2x 2x", None, "sx-1x", "sx-2x"),
            (Some("sy-src"), "sy-2x 2x", None, "sy-src", "sy-2x"),
            (Some("sz-src"), "sz-15 1.5x", None, "sz-src", "sz-15"),
            (None, "sa-15 1.5x, sa-4 4x", None, "sa-15", "sa-4"),
            (None, "sb-1, sb-25 2.5x", None, "sb-1", "sb-25"),
            (None, "sc-05 0.5x, sc-15 1.5x", None, "sc-15", "sc-15"),
            (None, "sd-08 0.8x, sd-3 3x", None, "sd-3", "sd-3"),
            (None, "se-3 3x, se-1 1x", None, "se-1", "se-3"),
            (None, "sf-09 0.9x, sf-11 1.1x", None, "sf-11", "sf-11"),
            (None, "sg-a 1x, sg-b 1x", None, "sg-a", "sg-a"),
            (None, "w-400 400w, w-1600 1600w", Some("100px"), "w-400", "w-400"),
            (Some("hh-src"), "hh-500 500w, hh-800 800w, hh-1080 1080w, hh-1600 1600w, hh-2000 2000w",
             Some("(max-width: 479px) 85vw, (max-width: 767px) 63vw, (max-width: 991px) 33vw, 32vw"), "hh-800", "hh-1600"),
            (None, "vw-1905 1905w, vw-1915 1915w, vw-1925 1925w", None, "vw-1925", "vw-1925"),
            (None, "nz-1000 1000w, nz-2000 2000w, nz-4000 4000w", None, "nz-2000", "nz-4000"),
            (None, "cz-900 900w, cz-1000 1000w", Some("calc(50vw - 10px)"), "cz-1000", "cz-1000"),
            (None, "em-100 100w, em-170 170w", Some("10em"), "em-170", "em-170"),
            (None, "mc-280 280w, mc-310 310w", Some("(min-width: 5000px) 10px, 300px"), "mc-310", "mc-310"),
            (None, "bg-190 190w, bg-210 210w", Some("bogus, 200px"), "bg-210", "bg-210"),
            (Some("mix-src"), "mix-w 500w, mix-x 2x", None, "mix-x", "mix-x"),
        ];
        for (src, srcset, sizes, at1, at2) in rows {
            assert_eq!(select(*src, Some(srcset), *sizes, &ctx(1.0)).as_deref(), Some(*at1), "DPR 1: {srcset}");
            assert_eq!(select(*src, Some(srcset), *sizes, &ctx(2.0)).as_deref(), Some(*at2), "DPR 2: {srcset}");
        }
    }

    #[test]
    fn a_bad_descriptor_drops_only_its_candidate() {
        let c = parse_srcset("a.png 1x 2x, b.png 100w, c.png foo, d.png, e.png 2x");
        let urls: Vec<&str> = c.iter().map(|c| c.url.as_str()).collect();
        assert_eq!(urls, ["b.png", "d.png", "e.png"]);
    }
}
