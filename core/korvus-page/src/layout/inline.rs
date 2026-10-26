//! Text in the layout: whitespace collapsing, line breaking, and how tall a paragraph is.
//!
//! # What this is and is not
//!
//! It is enough for a block containing text to have a *height*, and for an atomic inline to have a
//! *width*. That closes the cross-check a detector runs -- `ctx.measureText(s).width` against the
//! `offsetWidth` of an element containing the same text -- and it turns "every paragraph is zero
//! pixels tall" into a page with plausible geometry.
//!
//! It is not a full inline formatting context. An inline `<span>` still has no box of its own, so
//! its own `getBoundingClientRect` is zero; real per-fragment rects need line boxes that remember
//! which element contributed which run, which is the rest of P3. The shape for it is already in
//! place -- `client_rects` returns a list (design D11) -- so that lands without a caller changing.
//!
//! # Whitespace is not decoration
//!
//! HTML is written with newlines and indentation, and CSS collapses them: a run of whitespace is
//! one space, and a text node that is *only* whitespace between two blocks generates nothing at
//! all. Skipping this does not make text slightly wider, it makes every indented document's
//! measurements meaningless.

use crate::font::{FontMetrics, FontProvider};
use taffy::geometry::Size;

/// The font a run of text is measured in.
#[derive(Debug, Clone, PartialEq)]
pub struct FontSpec {
    pub families: Box<str>,
    pub size: f32,
    /// `None` means `line-height: normal` — the font's own line spacing (ascent + descent + gap).
    pub line_height: Option<f32>,
    /// Whether whitespace is preserved (`pre`, `pre-wrap`, `break-spaces`) rather than collapsed.
    pub preserve: bool,
    /// Whether lines may wrap (`text-wrap-mode: wrap`).
    pub wrap: bool,
    /// The lines are balanced (`text-wrap-style: balance`): a paragraph of a few lines breaks at the narrowest width
    /// that keeps their number.
    pub balance: bool,
    /// The bold face: a computed `font-weight` of 600 or more.
    pub bold: bool,
    /// The computed `font-weight` itself (1 to 1000), which picks among a web font's faces.
    pub weight: u16,
    /// The italic face: `font-style: italic` or `oblique`.
    pub italic: bool,
    /// The font's kerning pairs apply: every `font-kerning` but `none`.
    pub kerning: bool,
    /// The computed `letter-spacing` and `word-spacing`, in px (`normal` is 0).
    pub letter_spacing: f32,
    pub word_spacing: f32,
    /// The computed `text-transform`: the case the text is laid out and painted in ([`TextTransform::apply`]).
    pub transform: TextTransform,
}

/// The `text-transform` a text box's characters are laid out and painted through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextTransform {
    #[default]
    None,
    Uppercase,
    Lowercase,
    Capitalize,
}

impl TextTransform {
    pub fn of(value: &str) -> TextTransform {
        match value.trim() {
            "uppercase" => TextTransform::Uppercase,
            "lowercase" => TextTransform::Lowercase,
            "capitalize" => TextTransform::Capitalize,
            _ => TextTransform::None,
        }
    }

    /// `text` in this case, character for character and byte for byte: a character whose mapping is not one
    /// character of the same length (`ß` -> `SS`) keeps its own, so every DOM offset into the text still lands
    /// where it did. `capitalize` takes a word's first letter up -- a word starts the text and follows white space
    /// or punctuation other than an apostrophe, and one that starts with a digit is left alone (Chrome 150:
    /// `hello  world 3rd o'neil x-ray` paints `Hello World 3rd O'neil X-Ray`).
    pub fn apply(self, text: &str) -> std::borrow::Cow<'_, str> {
        let same = |c: char, mut m: Box<dyn Iterator<Item = char>>| match (m.next(), m.next()) {
            (Some(u), None) if u.len_utf8() == c.len_utf8() => u,
            _ => c,
        };
        match self {
            TextTransform::None => std::borrow::Cow::Borrowed(text),
            TextTransform::Uppercase => text.chars().map(|c| same(c, Box::new(c.to_uppercase()))).collect::<String>().into(),
            TextTransform::Lowercase => text.chars().map(|c| same(c, Box::new(c.to_lowercase()))).collect::<String>().into(),
            TextTransform::Capitalize => {
                let mut start = true;
                text.chars()
                    .map(|c| {
                        let out = if start && c.is_alphabetic() { same(c, Box::new(c.to_uppercase())) } else { c };
                        start = c.is_whitespace() || (c.is_ascii_punctuation() && c != '\'') || matches!(c, '\u{2010}'..='\u{2015}');
                        out
                    })
                    .collect::<String>()
                    .into()
            }
        }
    }
}

impl Default for FontSpec {
    fn default() -> Self {
        FontSpec {
            families: "".into(),
            size: 16.0,
            line_height: None,
            preserve: false,
            wrap: true,
            balance: false,
            bold: false,
            weight: 400,
            italic: false,
            kerning: true,
            letter_spacing: 0.0,
            word_spacing: 0.0,
            transform: TextTransform::None,
        }
    }
}

impl FontSpec {
    /// The width `text` advances in this font, as layout measures a run: [`FontMetrics::width_px_kerned`].
    pub fn advance_of(&self, metrics: &FontMetrics, text: &str) -> f32 {
        metrics.width_px_kerned(text, self.size, self.kerning) + self.spacing(text)
    }

    /// The `letter-spacing` after every character of `text`, its last included, and the `word-spacing` after each
    /// space (Blink's `ShapeResultSpacing`; a character that joins the one before it takes none). Chrome 150 lays
    /// `Find Your Next` in 600 64px Inter out 453.75 wide, and 435.84 under `letter-spacing: -1.28px`.
    pub fn spacing(&self, text: &str) -> f32 {
        if self.letter_spacing == 0.0 && self.word_spacing == 0.0 {
            return 0.0;
        }
        text.chars()
            .filter(|&c| !crate::font::zero_width(c))
            .map(|c| self.letter_spacing + if matches!(c, ' ' | '\u{a0}' | '\t') { self.word_spacing } else { 0.0 })
            .sum()
    }

    /// The height of one line box.
    ///
    /// `normal` is the font's **line spacing**: ascent + descent + line gap, with each of the three
    /// rounded to a whole pixel *separately* and only then summed. That last detail is the whole
    /// rule, and it was measured rather than reasoned — 165 line boxes read back from the control
    /// Brave 151 (five families × sizes 8 through 40) agree with this expression exactly, and
    /// disagree with a single rounding of the sum on 22 of 33 sizes for Arial alone. `round((a+d)*s)`
    /// gets 16px Arial right by luck (17.87 → 18) and 24px Arial wrong by one (26.8 → 27, not 28),
    /// so the error is invisible at the one size anybody spot-checks.
    ///
    /// It is the same arithmetic Blink does: Skia hands it a float ascent/descent/leading, Blink
    /// rounds each to an integer pixel when it builds the font's metrics, and the line spacing is
    /// the sum of the three rounded numbers.
    ///
    /// The size is quantized to 1/64 px first, for the reason [`crate::font::used_size`] gives.
    pub fn line_height(&self, metrics: &FontMetrics) -> f32 {
        self.line_height.unwrap_or_else(|| {
            let size = crate::font::used_size(self.size);
            (metrics.ascent * size).round()
                + (metrics.descent * size).round()
                + (metrics.line_gap * size).round()
        })
    }
}

/// Collapse whitespace the way CSS does: every run of it becomes one space.
///
/// Returns `None` for a text node that contributes nothing — one that is entirely whitespace, which
/// is what sits between two block elements in any indented document.
pub fn collapse(text: &str, preserve: bool) -> Option<String> {
    if preserve {
        return (!text.is_empty()).then(|| text.to_string());
    }
    if text.chars().all(is_collapsible) {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for ch in text.chars() {
        if is_collapsible(ch) {
            in_space = true;
            continue;
        }
        if in_space && !out.is_empty() {
            out.push(' ');
        }
        in_space = false;
        out.push(ch);
    }
    // A leading or trailing space survives as a single space, because it separates this run from a
    // neighbouring one. Collapsing it away entirely would run two words together.
    if text.starts_with(is_collapsible) {
        out.insert(0, ' ');
    }
    if text.ends_with(is_collapsible) {
        out.push(' ');
    }
    Some(out)
}

/// Collapse whitespace for a run that is going ON A LINE, where a space between two boxes is
/// content rather than indentation.
///
/// The difference from [`collapse`] is one case and it is the one that matters most often: a text
/// node that is ENTIRELY whitespace. Between two block elements it generates nothing, which is what
/// `collapse` says and why every indented document does not gain a blank line per tag. Between two
/// inline ones it is the space that separates them, and dropping it runs them together — Chrome
/// measures the ` ` between `</a>` and `<span>` at 4.45px and starts the span 4.45px later.
///
/// A space that turns out to be at a line's start or end is dropped by the line breaker, which is
/// where that decision belongs: it cannot be made here, before it is known which line the run
/// lands on.
pub fn collapse_inline(text: &str, preserve: bool) -> Option<String> {
    if let Some(collapsed) = collapse(text, preserve) {
        return Some(collapsed);
    }
    (!preserve && !text.is_empty()).then(|| " ".to_string())
}

/// Where each character of [`collapse_inline`]'s output came from in the source text, as a UTF-16
/// offset, plus one final entry for the end of the text: `out[k]` is the DOM offset a caret before
/// collapsed character `k` names (`caretPositionFromPoint`).
///
/// It walks the text exactly as [`collapse`] does: a run of collapsible characters becomes one space
/// that stands for the run's FIRST character, a leading or trailing run survives as one space, and a
/// text that is all white space is one space standing for its start.
pub fn collapsed_offsets(text: &str, preserve: bool) -> Vec<u32> {
    let mut out = Vec::new();
    let mut pos: u32 = 0;
    if preserve || !text.chars().all(is_collapsible) {
        if preserve {
            for ch in text.chars() {
                out.push(pos);
                pos += ch.len_utf16() as u32;
            }
            out.push(pos);
            return out;
        }
        let leading = text.starts_with(is_collapsible);
        if leading {
            out.push(0);
        }
        let mut content = 0usize;
        let mut run_start: Option<u32> = None;
        for ch in text.chars() {
            if is_collapsible(ch) {
                if run_start.is_none() {
                    run_start = Some(pos);
                }
                pos += ch.len_utf16() as u32;
                continue;
            }
            if let Some(start) = run_start.take() {
                if content > 0 {
                    out.push(start);
                }
            }
            out.push(pos);
            content += 1;
            pos += ch.len_utf16() as u32;
        }
        if let Some(start) = run_start {
            out.push(start);
        }
        out.push(pos);
        return out;
    }
    if !text.is_empty() {
        out.push(0);
    }
    out.push(text.encode_utf16().count() as u32);
    out
}

/// Is this one of the characters CSS collapses?
///
/// **Not `char::is_whitespace`**, which is Unicode's `White_Space` property and includes U+00A0.
/// A non-breaking space is exactly the character an author writes to stop this happening, and
/// Rust's predicate would collapse it away: `x<span>&nbsp;&nbsp;</span>y` measured 0 wide here and
/// **8.89** in Chrome 150 — two space advances, neither collapsed. CSS Text 3 §White Space
/// Processing names four characters and U+00A0 is not among them.
pub(crate) fn is_collapsible(ch: char) -> bool {
    matches!(ch, ' ' | '\t' | '\n' | '\r' | '\u{c}')
}

/// One laid-out line: its text and how wide it is.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub text: String,
    pub width: f32,
}

/// Break `text` into lines that fit in `available`, and measure each.
///
/// Breaks at spaces only. That is what CSS does for `word-break: normal` in Latin script, and it is
/// why a single word longer than the line overflows rather than being cut: `overflow-wrap: normal`
/// says so, and a browser that broke it anyway would report a narrower box than the real one.
pub fn break_lines(
    metrics: &FontMetrics,
    spec: &FontSpec,
    text: &str,
    available: Option<f32>,
) -> Vec<Line> {
    // A line's text is one run, as wide as Blink's `ShapeResult::SnappedWidth` makes it: its exact advance,
    // snapped UP to a `LayoutUnit` (Chrome 150: five 16px Noto Sans Mono CJK glyphs in a flex item are
    // 40.015625 wide). Where the lines break is still decided on the linear sums below.
    let width_of = |s: &str| (spec.advance_of(metrics, s) * 64.0).ceil() / 64.0;

    // `pre` keeps the author's own line breaks and adds none; `pre-wrap` and `break-spaces` keep them and still wrap
    // each line at its spaces (it was laid out as `pre`: Framer's `pre-wrap` headings ran off the page on one line).
    if spec.preserve {
        if !spec.wrap {
            return text
                .split('\n')
                .map(|line| Line { text: line.to_string(), width: width_of(line) })
                .collect();
        }
        let collapsing = FontSpec { preserve: false, ..spec.clone() };
        return text.split('\n').flat_map(|line| break_lines(metrics, &collapsing, line, available)).collect();
    }
    let limit = match (available, spec.wrap) {
        // a limit of 0 -- the min-content question -- breaks at every opportunity: Chrome 150 lays a flex
        // item of bare text in a 0-wide flex box out one word per line, where this kept it on one
        (Some(limit), true) if limit >= 0.0 => limit,
        // Nothing to wrap against, or `nowrap`: one line however long it is.
        _ => return vec![Line { text: text.to_string(), width: width_of(text) }],
    };

    let size = crate::font::used_size(spec.size);
    let mut lines = Vec::new();
    let mut current = String::new();
    // The line measured so far: `whole` through its last character, `trimmed` through its
    // last non-white-space one. Each word EXTENDS them; re-measuring the line's whole prefix per word
    // (and building it with `format!`) was O(words^2) per line -- 15 % of hubspot.com's load CPU,
    // measured 2026-09-29. `FontMetrics::extend` adds in `width`'s own order, so every width here
    // is the identical float and no break moves.
    let mut whole = crate::font::Run::default();
    let mut trimmed = crate::font::Run::default();
    // the letter- and word-spacing of `whole` and of `trimmed`, beside their advances
    let (mut whole_spaced, mut trimmed_spaced) = (0.0f32, 0.0f32);
    for word in text.split_inclusive(' ') {
        let body = word.trim_end();
        let tail = &word[body.len()..];
        // `trim_end` because a trailing space does not have to fit: it hangs past the line's end,
        // which is what every browser does and why a line of text that just fits is not wrapped by
        // the space after its last word.
        let candidate = if body.is_empty() { trimmed } else { metrics.extend(whole, body) };
        let candidate_spaced = if body.is_empty() { trimmed_spaced } else { whole_spaced + spec.spacing(body) };
        if !current.is_empty() && candidate.total * size + candidate_spaced > limit {
            let text = current.trim_end().to_string();
            lines.push(Line { width: width_of(&text), text });
            current = word.to_string();
            trimmed = metrics.extend(crate::font::Run::default(), body);
            whole = metrics.extend(trimmed, tail);
            trimmed_spaced = spec.spacing(body);
            whole_spaced = trimmed_spaced + spec.spacing(tail);
        } else {
            current.push_str(word);
            let through_body = if body.is_empty() { whole } else { candidate };
            let through_body_spaced = if body.is_empty() { whole_spaced } else { candidate_spaced };
            if !body.is_empty() {
                trimmed = candidate;
                trimmed_spaced = candidate_spaced;
            }
            whole = metrics.extend(through_body, tail);
            whole_spaced = through_body_spaced + spec.spacing(tail);
        }
    }
    let last = current.trim_end();
    if !last.is_empty() || lines.is_empty() {
        lines.push(Line { text: last.to_string(), width: width_of(last) });
    }
    lines
}

/// The size a run of text occupies: the widest line, and the lines stacked.
pub fn measure_block(
    provider: &dyn FontProvider,
    spec: &FontSpec,
    text: &str,
    available: Option<f32>,
) -> Size<f32> {
    let metrics = provider.metrics_at(&spec.families, spec.weight, spec.italic, spec.size);
    let lines = break_lines(metrics, spec, text, available);
    let width = lines.iter().map(|l| l.width).fold(0.0f32, f32::max);
    Size { width, height: lines.len() as f32 * spec.line_height(metrics) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::BuiltinFonts;

    fn arial(size: f32) -> FontSpec {
        FontSpec { families: "Arial".into(), size, ..Default::default() }
    }

    fn metrics() -> &'static FontMetrics {
        BuiltinFonts.metrics("Arial")
    }

    // `break_lines` extends each line's width word by word, where the breaker before it
    // re-measured the line's whole prefix per word. That breaker, kept here as the oracle, must give
    // the SAME lines with bit-identical widths -- the same additions in the same order -- on texts with
    // kerning pairs, flags, NBSP, tabs and runs of spaces, at every limit.
    #[test]
    fn text_transform_keeps_every_offset() {
        assert_eq!(TextTransform::Uppercase.apply("straße mañana"), "STRAßE MAÑANA");
        assert_eq!(TextTransform::Lowercase.apply("SEE ALL"), "see all");
        assert_eq!(TextTransform::Capitalize.apply("hello  world 3rd o'neil x-ray"), "Hello  World 3rd O'neil X-Ray");
        assert_eq!(TextTransform::None.apply("As Is"), "As Is");
        for t in [TextTransform::Uppercase, TextTransform::Lowercase, TextTransform::Capitalize] {
            let s = "ıstanbul İzmir ǆ ß ﬁ";
            assert_eq!(t.apply(s).len(), s.len());
        }
    }

    #[test]
    fn spacing_follows_every_character_and_each_space() {
        let spec = FontSpec { letter_spacing: 2.0, word_spacing: 5.0, ..arial(16.0) };
        // seven characters, one space; a combining mark takes none
        assert_eq!(spec.spacing("ab cdef"), 7.0 * 2.0 + 5.0);
        assert_eq!(spec.spacing("e\u{301}"), 2.0);
        assert_eq!(spec.advance_of(metrics(), "ab cdef"), arial(16.0).advance_of(metrics(), "ab cdef") + 19.0);
        // a line breaks where the spaced words stop fitting
        let plain = break_lines(metrics(), &arial(16.0), "aaaa bbbb cccc", Some(80.0));
        let spaced = break_lines(metrics(), &FontSpec { letter_spacing: 4.0, ..arial(16.0) }, "aaaa bbbb cccc", Some(80.0));
        assert!(spaced.len() > plain.len(), "{plain:?} {spaced:?}");
    }

    #[test]
    fn word_by_word_widths_are_the_prefix_measures_bit_for_bit() {
        fn prefix_breaker(metrics: &FontMetrics, spec: &FontSpec, text: &str, limit: f32) -> Vec<(String, u32)> {
            // where to break: the linear sums; what a line is wide: its run, snapped up to a layout unit
            let fits = |s: &str| metrics.width(s) * crate::font::used_size(spec.size);
            let width_of = |s: &str| (spec.advance_of(metrics, s) * 64.0).ceil() / 64.0;
            let mut lines = Vec::new();
            let mut current = String::new();
            for word in text.split_inclusive(' ') {
                let candidate = format!("{current}{word}");
                if !current.is_empty() && fits(candidate.trim_end()) > limit {
                    lines.push((current.trim_end().to_string(), width_of(current.trim_end()).to_bits()));
                    current = word.to_string();
                } else {
                    current = candidate;
                }
            }
            let last = current.trim_end();
            if !last.is_empty() || lines.is_empty() {
                lines.push((last.to_string(), width_of(last).to_bits()));
            }
            lines
        }
        let spec = FontSpec { wrap: true, ..arial(16.0) };
        for text in [
            "AVATAR To Ya WAVE Tj Yo LT AV AVATAR To Ya",
            "The quick brown fox jumps over the lazy dog, again and again and again.",
            "a  b   c    d\u{a0}e\u{a0} f\tg \u{1F1F0}\u{1F1F7} \u{1F1FA}\u{1F1F8}x y",
            "   leading and trailing   ",
            " ",
            "",
            "supercalifragilisticexpialidocious short words after a very long one",
        ] {
            for limit in [1.0, 20.0, 37.5, 60.0, 100.0, 150.0, 333.3, 1000.0] {
                let lines: Vec<(String, u32)> =
                    break_lines(metrics(), &spec, text, Some(limit)).into_iter().map(|l| (l.text, l.width.to_bits())).collect();
                assert_eq!(lines, prefix_breaker(metrics(), &spec, text, limit), "{text:?} at {limit}");
            }
        }
    }

    // Every collapsed character maps back to where it came from, and the map has one
    // entry per collapsed character plus the end -- the invariant a caret offset relies on.
    #[test]
    fn collapsed_offsets_point_back_into_the_source() {
        for (text, preserve) in [("hello world", false), ("  a \n\t b  ", false), ("   ", false), ("x\u{a0} y", false), ("a\nb", true), ("é😀 z", false)] {
            let collapsed = collapse_inline(text, preserve).unwrap_or_default();
            let offsets = collapsed_offsets(text, preserve);
            assert_eq!(offsets.len(), collapsed.chars().count() + 1, "{text:?} -> {collapsed:?}: {offsets:?}");
            assert!(offsets.windows(2).all(|w| w[0] <= w[1]), "{text:?}: {offsets:?}");
            assert_eq!(*offsets.last().unwrap(), text.encode_utf16().count() as u32, "{text:?}");
        }
        // `  a \n\t b  ` collapses to ` a b `: the leading space stands for offset 0, `a` is at 2, the
        // run after it for its first character (3), `b` at 7, the trailing run at 8, and the end is 10.
        assert_eq!(collapsed_offsets("  a \n\t b  ", false), vec![0, 2, 3, 7, 8, 10]);
        // An astral character is two UTF-16 units.
        assert_eq!(collapsed_offsets("é😀 z", false), vec![0, 1, 3, 4, 5]);
    }

    // The case every indented document is full of.
    #[test]
    fn whitespace_between_blocks_contributes_nothing() {
        assert_eq!(collapse("\n    ", false), None);
        assert_eq!(collapse("\n\t\n", false), None);
        assert_eq!(collapse("", false), None);
    }

    #[test]
    fn a_run_of_whitespace_becomes_one_space() {
        assert_eq!(collapse("a   b", false).as_deref(), Some("a b"));
        assert_eq!(collapse("a\n\t  b\n c", false).as_deref(), Some("a b c"));
        assert_eq!(collapse("Hello,\n  World!", false).as_deref(), Some("Hello, World!"));
    }

    // A leading or trailing space separates this run from a neighbouring one. Dropping it would run
    // `<b>one</b> <b>two</b>` together as "onetwo".
    #[test]
    fn a_leading_or_trailing_space_survives_as_one_space() {
        assert_eq!(collapse("  hello  ", false).as_deref(), Some(" hello "));
        assert_eq!(collapse("\n\thello", false).as_deref(), Some(" hello"));
    }

    #[test]
    fn preserved_whitespace_is_left_exactly_as_written() {
        assert_eq!(collapse("a   b", true).as_deref(), Some("a   b"));
        assert_eq!(collapse("\n    ", true).as_deref(), Some("\n    "));
        assert_eq!(collapse("", true), None);
    }

    #[test]
    fn text_that_fits_is_one_line() {
        let lines = break_lines(metrics(), &arial(16.0), "Hello, World!", Some(500.0));
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "Hello, World!");
        assert!((lines[0].width - 91.289).abs() < 0.01, "the same number measureText gives");
    }

    #[test]
    fn text_that_does_not_fit_breaks_at_a_space() {
        let text = "The quick brown fox jumps over the lazy dog";
        let lines = break_lines(metrics(), &arial(16.0), text, Some(120.0));
        assert!(lines.len() > 2, "got {lines:?}");
        for line in &lines {
            assert!(!line.text.starts_with(' ') || line.text.trim().is_empty());
            assert!(!line.text.ends_with(' '));
        }
        // Nothing is lost and nothing is duplicated.
        let rejoined: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(rejoined.join(" "), text);
    }

    // `overflow-wrap: normal` says a word longer than its line overflows rather than being cut, and
    // a browser that cut it would report a narrower box than the real one.
    #[test]
    fn a_word_longer_than_the_line_overflows_rather_than_being_cut() {
        let lines = break_lines(metrics(), &arial(16.0), "Supercalifragilistic", Some(20.0));
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "Supercalifragilistic");
        assert!(lines[0].width > 20.0);
    }

    #[test]
    fn nowrap_and_an_unbounded_line_both_give_one_line() {
        let text = "The quick brown fox jumps over the lazy dog";
        let nowrap = FontSpec { wrap: false, ..arial(16.0) };
        assert_eq!(break_lines(metrics(), &nowrap, text, Some(50.0)).len(), 1);
        assert_eq!(break_lines(metrics(), &arial(16.0), text, None).len(), 1);
    }

    // `pre` keeps the author's breaks and adds none of its own (`white-space: pre` is `text-wrap-mode: nowrap`).
    #[test]
    fn preserved_text_breaks_where_the_author_did_and_nowhere_else() {
        let spec = FontSpec { preserve: true, wrap: false, ..arial(16.0) };
        let lines = break_lines(metrics(), &spec, "one\ntwo three four five six", Some(30.0));
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "one");
        assert!(lines[1].width > 30.0, "no wrapping, so it overflows");
    }

    // `pre-wrap` keeps the author's breaks and wraps each line at its spaces as well.
    #[test]
    fn pre_wrap_text_breaks_where_the_author_did_and_where_it_must() {
        let spec = FontSpec { preserve: true, ..arial(16.0) };
        let lines = break_lines(metrics(), &spec, "one\ntwo three four", Some(50.0));
        assert_eq!(lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["one", "two", "three", "four"]);
    }

    // A line box is taller than the font size -- Arial at 16px is 18px -- which is why a
    // paragraph's height is never a round multiple of its font size.
    #[test]
    fn a_line_box_is_taller_than_the_font_size() {
        let spec = arial(16.0);
        let h = spec.line_height(metrics());
        assert_eq!(h, 18.0);
        assert!(h > spec.size);

        let fixed = FontSpec { line_height: Some(24.0), ..arial(16.0) };
        assert_eq!(fixed.line_height(metrics()), 24.0);
    }

    /// `line-height: normal`, against the control browser, at every size a page is likely to use.
    ///
    /// This is the test the one-size spot check could not be. 16px Arial is 18px under BOTH the old
    /// rule (`round((ascent + descent) * size)` = round(17.87) = 18) and the right one (14 + 3 + 1),
    /// so the size everybody checks was the size that hid the bug: the old rule was wrong on 22 of
    /// these 33 sizes for Arial alone, and by two pixels at 26px and 36px.
    ///
    /// The three rows are read back from the control Brave 151 on this machine — the offsetHeight of
    /// a `<div>` holding one line of text, per size, per family. Nothing here is derived from our
    /// own arithmetic, which is the point: they are the browser's answers, and the implementation
    /// has to land on them.
    #[test]
    fn normal_line_height_matches_the_browser_at_every_size() {
        const FIRST: usize = 8;
        #[rustfmt::skip]
        const RECORDED: [(&str, [f32; 33]); 3] = [
            ("Arial", [9., 10., 11., 12., 14., 15., 16., 17., 18., 20., 21., 22., 23., 24., 26.,
                       27., 28., 29., 31., 31., 32., 33., 34., 36., 37., 38., 39., 40., 42., 42.,
                       43., 44., 45.]),
            ("Times New Roman", [9., 10., 11., 12., 15., 16., 16., 17., 18., 20., 21., 22., 23.,
                       25., 26., 26., 27., 28., 30., 31., 32., 33., 34., 36., 37., 37., 38., 40.,
                       42., 43., 44., 45., 47.]),
            ("Courier New", [9., 10., 11., 12., 14., 15., 16., 17., 18., 19., 20., 22., 23., 23.,
                       25., 26., 27., 29., 30., 30., 31., 33., 34., 35., 37., 37., 38., 40., 41.,
                       42., 43., 44., 45.]),
        ];
        for (family, heights) in RECORDED {
            let metrics = BuiltinFonts.metrics(family);
            for (i, expected) in heights.iter().enumerate() {
                let size = (FIRST + i) as f32;
                let spec = FontSpec { families: family.into(), size, ..FontSpec::default() };
                assert_eq!(
                    spec.line_height(metrics),
                    *expected,
                    "{family} at {size}px: the control Brave says {expected}"
                );
            }
        }
    }

    /// The control's `document.body.offsetHeight` of 24 falls out of these metrics and this rule.
    ///
    /// It is worth pinning because the number looks like a line-height bug and is not one. The
    /// control Brave reports 24 for a default document and 18 for `16px Arial` -- and we report 18
    /// for `16px Arial` too. The whole difference is which family a document with no `font-family`
    /// uses: on the control's `ko_KR` Linux host, Chrome's Standard-font preference resolves to Noto
    /// Sans CJK KR, whose metrics are read here from the control itself at 2048px, where its
    /// reported integers are the font's design units (ascent 2376, descent 590, line box 2966, so no
    /// line gap).
    ///
    /// The persona's own face now: `font_table.rs` carries Noto Sans CJK KR as the
    /// persona's browser measured it, and the persona's standard font IS that face, so a default
    /// document lays out at the persona's 24 -- the arithmetic, with no constant to hardcode.
    #[test]
    fn the_controls_default_font_produces_its_24px_line_box() {
        let noto = crate::font::resolve("Noto Sans CJK KR").expect("the persona has Noto Sans CJK KR");
        let spec = FontSpec { families: "Noto Sans CJK KR".into(), size: 16.0, ..FontSpec::default() };
        assert_eq!(spec.line_height(noto), 24.0, "19 + 5 + 0, and the control says 24");
        // And the sizes around it, so this is the rule and not one lucky number.
        for (size, expected) in [(8.0, 11.0), (12.0, 17.0), (20.0, 29.0), (32.0, 46.0)] {
            let spec = FontSpec { size, ..spec.clone() };
            assert_eq!(spec.line_height(noto), expected, "at {size}px");
        }
        // A document with no font-family gets it: the default face IS this one.
        let default = BuiltinFonts.metrics("");
        assert_eq!(default.family, noto.family);
    }

    #[test]
    fn a_block_of_text_is_as_tall_as_its_lines() {
        let text = "The quick brown fox jumps over the lazy dog";
        let one = measure_block(&BuiltinFonts, &arial(16.0), text, Some(1000.0));
        assert_eq!(one.height, 18.0);
        assert!((one.width - 316.6).abs() < 1.0, "got {}", one.width);

        let many = measure_block(&BuiltinFonts, &arial(16.0), text, Some(120.0));
        assert!(many.height >= 54.0, "three lines or more: {}", many.height);
        assert!(many.width <= 120.0, "and none of them wider than the box");
    }
}
