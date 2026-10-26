//! Replaced elements and form controls: the boxes whose size comes from what they ARE rather than
//! from what is inside them.
//!
//! # The defect this closes
//!
//! Every box here used to be a taffy leaf with no text, and a leaf with no text measured zero. So a
//! bare `<input>` was 0×0 — a probe looking for a field to type into found nothing to aim at — a
//! `<canvas>` was 0 wide, so a page that sizes its drawing buffer from its box drew nothing, and
//! every `<img>`/`<svg>` icon sat on a 0-wide strut 17px tall. Measured on Chrome 150.0.7871.46,
//! `getBoundingClientRect()` of each in `<body>` with only the UA sheet, 1920-wide viewport:
//!
//! ```text
//! <input>                                 korvus   0x0     chrome 185x21
//! <textarea>                              korvus   0x0     chrome 155x44
//! <img width=40 height=30 src=gif>        korvus   0x17    chrome  40x30
//! <img> (no src, no size)                 korvus   0x17    chrome   0x0
//! <canvas>                                korvus   0x17    chrome 300x150
//! <video>                                 korvus   0x17    chrome 300x150
//! <svg>                                   korvus   0x17    chrome 300x150
//! ```
//!
//! # Two questions, kept apart
//!
//! 1. **What is the thing's natural size?** — [`Natural`]. A canvas is its `width`/`height`
//!    attributes (300×150 when absent: HTML's own defaults). A video with no metadata read and an
//!    iframe have none. An outer `<svg>` has whatever its `width`/`height` say plus a ratio from its
//!    `viewBox`. A text field is a number of average characters wide and one line tall, in the
//!    persona's font. This is per element and lives in `tree.rs`, which can see the DOM.
//! 2. **Given that, and the style, how big is the box?** — [`compute_replaced_layout`]. That is CSS
//!    2.1 §10.3.2 / §10.6.2 (auto sizes from the natural ones or the ratio, else the default object
//!    size 300×150) and the §10.4 min/max table, and it knows nothing about which element it sizes.
//!
//! 300×150 is not a browser's number: it is CSS 2.1's "the used value of 'width' becomes 300px" and
//! CSS Images 3's default object size, the same in every engine.

use crate::font::FontMetrics;
use crate::layout::inline::FontSpec;
use taffy::geometry::{Rect, Size};
use taffy::style::{AvailableSpace, BoxSizing, Style};
use taffy::tree::{LayoutInput, LayoutOutput, SizingMode};
use taffy::{MaybeResolve, ResolveOrZero};

/// CSS 2.1 §10.3.2 / CSS Images 3 §default object size.
pub const DEFAULT_OBJECT_WIDTH: f32 = 300.0;
/// CSS 2.1 §10.6.2 / CSS Images 3 §default object size.
pub const DEFAULT_OBJECT_HEIGHT: f32 = 150.0;

/// A replaced element's natural dimensions, in CSS pixels of CONTENT box. Any of the three may be
/// absent, and which ones are is the whole input to CSS's sizing rules.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Natural {
    pub width: Option<f32>,
    pub height: Option<f32>,
    /// width / height.
    pub ratio: Option<f32>,
}

impl Natural {
    /// Both dimensions known, and so the ratio between them when neither is zero.
    pub fn sized(width: f32, height: f32) -> Natural {
        Natural {
            width: Some(width),
            height: Some(height),
            ratio: (width > 0.0 && height > 0.0).then(|| width / height),
        }
    }

    /// The natural ratio as a `(width, height)` pair: the natural size itself when the ratio is that
    /// size's, so one axis follows the other as `w * h / w` -- exactly, as Blink's arithmetic does.
    fn ratio_pair(&self) -> Option<(f32, f32)> {
        let r = self.ratio.filter(|r| r.is_finite() && *r > 0.0)?;
        Some(match (self.width, self.height) {
            (Some(w), Some(h)) if h > 0.0 && w / h == r => (w, h),
            _ => (r, 1.0),
        })
    }
}

/// A box's `aspect-ratio`, as written: `width / height` (a bare number is `n / 1`), and whether it came
/// with `auto` -- `auto && <ratio>`, which a replaced element with a natural ratio of its own leaves for
/// that one (CSS Sizing 4 §aspect-ratio). `auto` alone, or a degenerate ratio, is no `PreferredRatio`.
/// The `width`/`height` presentational hints of an `<img>` or a `<video>` are `auto w / h`
/// (`style::hints`): Chrome 150 lays `<img width=60 height=60 style="height:auto">` out 60x90 once its
/// 2x3 image is in, and `<img width=40 height=30 style="height:auto">` with no image 40x30.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PreferredRatio {
    pub width: f32,
    pub height: f32,
    pub auto: bool,
}

impl PreferredRatio {
    /// The `aspect-ratio` value `value` (`auto`, `16 / 9`, `1.5`, `auto 40 / 30`, `40 / 30 auto`).
    pub fn parse(value: &str) -> Option<PreferredRatio> {
        let mut auto = false;
        let mut ratio = String::new();
        for word in value.split_ascii_whitespace() {
            if word.eq_ignore_ascii_case("auto") {
                auto = true;
            } else {
                ratio.push_str(word);
            }
        }
        let (width, height) = match ratio.split_once('/') {
            Some((w, h)) => (w.parse::<f32>().ok()?, h.parse::<f32>().ok()?),
            None => (ratio.parse::<f32>().ok()?, 1.0),
        };
        (width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0).then_some(PreferredRatio { width, height, auto })
    }
}

/// Size a replaced box. The replacement for `compute_leaf_layout`'s measure function, which cannot
/// express this: taffy's leaf never tells its measure function a *style* width, so "the height
/// follows the width through the ratio" has nowhere to happen. `preferred` is the box's `aspect-ratio`
/// (the layout style's `aspect_ratio` is its quotient, which keeps neither the pair nor the `auto`).
pub fn compute_replaced_layout(inputs: LayoutInput, style: &Style, natural: Natural, preferred: Option<PreferredRatio>) -> LayoutOutput {
    let LayoutInput { known_dimensions, parent_size, available_space, sizing_mode, .. } = inputs;
    // A math function's value against its basis (it was always 0).
    let calc = crate::style::math::resolve_ptr;
    let margin = style.margin.resolve_or_zero(parent_size.width, calc);
    let padding = style.padding.resolve_or_zero(parent_size.width, calc);
    let border = style.border.resolve_or_zero(parent_size.width, calc);
    let pb = Size {
        width: padding.left + padding.right + border.left + border.right,
        height: padding.top + padding.bottom + border.top + border.bottom,
    };
    // Everything below is in CONTENT-box pixels, which is what natural sizes are. A known dimension
    // (a flex item stretched, a parent that has decided) is a border box; a style size is whatever
    // `box-sizing` says it is.
    let known = Size {
        width: known_dimensions.width.map(|w| (w - pb.width).max(0.0)),
        height: known_dimensions.height.map(|h| (h - pb.height).max(0.0)),
    };
    let from_style = |value: Size<Option<f32>>| match style.box_sizing {
        BoxSizing::ContentBox => value,
        BoxSizing::BorderBox => Size {
            width: value.width.map(|w| (w - pb.width).max(0.0)),
            height: value.height.map(|h| (h - pb.height).max(0.0)),
        },
    };
    let (specified, min, max) = match sizing_mode {
        SizingMode::ContentSize => (known, Size::NONE, Size::NONE),
        SizingMode::InherentSize => {
            let size = from_style(style.size.maybe_resolve(parent_size, calc));
            let min = from_style(style.min_size.maybe_resolve(parent_size, calc));
            let max = from_style(style.max_size.maybe_resolve(parent_size, calc));
            (Size { width: known.width.or(size.width), height: known.height.or(size.height) }, min, max)
        }
    };
    // An author's `aspect-ratio` wins over the element's own -- unless it is `auto && <ratio>`, which
    // yields to a natural ratio (CSS Sizing 4 §aspect-ratio). Kept as a (width, height) pair: an axis
    // that follows the other is `w * h / w`, never `w / (w / h)`, which in f32 left a 10x150 canvas
    // 149.984px tall where Chrome 150 has 150.
    let written = preferred
        .map(|p| (p.width, p.height, p.auto))
        .or_else(|| style.aspect_ratio.filter(|r| r.is_finite() && *r > 0.0).map(|r| (r, 1.0, false)));
    let own = natural.ratio_pair();
    let (ratio, from_style) = match written {
        Some((w, h, true)) if own.is_none() => (Some((w, h)), true),
        Some((_, _, true)) => (own, false),
        Some((w, h, false)) => (Some((w, h)), true),
        None => (own, false),
    };
    let height_for = |w: f32, (rw, rh): (f32, f32)| w * rh / rw;
    let width_for = |h: f32, (rw, rh): (f32, f32)| h * rw / rh;
    let min_w = min.width.unwrap_or(0.0);
    let min_h = min.height.unwrap_or(0.0);
    let max_w = max.width.map_or(f32::INFINITY, |m| m.max(min_w));
    let max_h = max.height.map_or(f32::INFINITY, |m| m.max(min_h));
    let clamp_w = |w: f32| w.min(max_w).max(min_w);
    let clamp_h = |h: f32| h.min(max_h).max(min_h);

    let (width, height) = match (specified.width, specified.height) {
        (Some(w), Some(h)) => (clamp_w(w), clamp_h(h)),
        // §10.6.2: an auto height follows a used width through the ratio first, and only then falls
        // back to a natural height, then 150.
        (Some(w), None) => {
            let w = clamp_w(w);
            let h = match ratio {
                Some(r) => height_for(w, r),
                None => natural.height.unwrap_or(DEFAULT_OBJECT_HEIGHT),
            };
            (w, clamp_h(h))
        }
        (None, Some(h)) => {
            let h = clamp_h(h);
            let w = match ratio {
                Some(r) => width_for(h, r),
                None => natural.width.unwrap_or(DEFAULT_OBJECT_WIDTH),
            };
            (clamp_w(w), h)
        }
        (None, None) => {
            let (w, h) = match (natural.width, natural.height, ratio) {
                // An author ratio re-derives the height from the natural width.
                (Some(w), Some(_), Some(r)) if from_style => (w, height_for(w, r)),
                (Some(w), Some(h), _) => (w, h),
                (Some(w), None, Some(r)) => (w, height_for(w, r)),
                (None, Some(h), Some(r)) => (width_for(h, r), h),
                // A ratio and nothing else — an `<svg viewBox>` with no size — is as wide as the
                // containing block allows (CSS Sizing 3's stretch-fit), which is what Chrome gives
                // `<svg viewBox="0 0 10 5">` in a 1889px body: 1889 × 944.5.
                (None, None, Some(r)) => {
                    let w = stretch_fit(available_space.width, parent_size.width, margin, pb.width);
                    (w, height_for(w, r))
                }
                (w, h, None) => {
                    (w.unwrap_or(DEFAULT_OBJECT_WIDTH), h.unwrap_or(DEFAULT_OBJECT_HEIGHT))
                }
            };
            match ratio {
                Some(_) if w > 0.0 && h > 0.0 => constrain_with_ratio(w, h, min_w, max_w, min_h, max_h),
                _ => (clamp_w(w), clamp_h(h)),
            }
        }
    };

    let size = Size { width: width + pb.width, height: height + pb.height };
    let scrolls = style.overflow.x.is_scroll_container() || style.overflow.y.is_scroll_container();
    LayoutOutput::from_sizes(
        size,
        Rect {
            left: 0.0,
            right: padding.left + width + if scrolls { padding.right } else { 0.0 },
            top: 0.0,
            bottom: padding.top + height + if scrolls { padding.bottom } else { 0.0 },
        },
    )
}

/// The content width an auto-sized box with only a ratio stretches to.
fn stretch_fit(
    available: AvailableSpace,
    parent: Option<f32>,
    margin: Rect<f32>,
    pb: f32,
) -> f32 {
    let outer = match available {
        AvailableSpace::Definite(w) => Some(w),
        _ => parent,
    };
    match outer {
        Some(w) => (w - margin.left - margin.right - pb).max(0.0),
        None => DEFAULT_OBJECT_WIDTH,
    }
}

/// CSS 2.1 §10.4's table: `min-*`/`max-*` on a box with both sizes auto and a ratio, which is the
/// one case where clamping one axis must move the other. `max-width: 100%` on a 1200px photo in a
/// 600px column is this table's first row, and the most common line of CSS on the web.
fn constrain_with_ratio(w: f32, h: f32, min_w: f32, max_w: f32, min_h: f32, max_h: f32) -> (f32, f32) {
    if w > max_w && h > max_h {
        if max_w / w <= max_h / h {
            (max_w, (max_w * h / w).max(min_h))
        } else {
            ((max_h * w / h).max(min_w), max_h)
        }
    } else if w < min_w && h < min_h {
        if min_w / w <= min_h / h {
            ((min_h * w / h).min(max_w), min_h)
        } else {
            (min_w, (min_w * h / w).min(max_h))
        }
    } else if w < min_w && h > max_h {
        (min_w, max_h)
    } else if w > max_w && h < min_h {
        (max_w, min_h)
    } else if w > max_w {
        (max_w, (max_w * h / w).max(min_h))
    } else if w < min_w {
        (min_w, (min_w * h / w).min(max_h))
    } else if h > max_h {
        ((max_h * w / h).max(min_w), max_h)
    } else if h < min_h {
        ((min_h * w / h).min(max_w), min_h)
    } else {
        (w, h)
    }
}

/// The character widths a text control is sized in: Blink's `GetAvgCharWidth` and the maximum
/// width `TextFieldIntrinsicInlineSize` adds, as `(average, Some(maximum))` in px — or
/// `(advance of 0, None)` for a font Blink reads no usable maximum from.
///
/// `SimpleFontData::PlatformInit` sets `avg_char_width_` to the font's own average
/// (`SkFontMetrics::fAvgCharWidth`, which on Linux is the OS/2 `xAvgCharWidth`) and — when the
/// platform reports none, which **DirectWrite does: it leaves `fAvgCharWidth` 0 for every Windows
/// font** — to the advance of `'x'` (`WidthForGlyph('x')`). NOT the advance of `'0'`: that stood in
/// for the fallback before a Windows persona was worn, and it is right only for a monospace font,
/// where the two advances coincide. `max_char_width_` is `round(fMaxCharWidth)` on Windows and
/// `round(head bbox)` elsewhere; the persona's table carries both numbers as the capture read them
/// (`FontMetrics::avg_char_width` 0/absent is the DirectWrite case, and the engine supplies the
/// `'x'` fallback Blink's own code does). Measured on Chrome 150 Linux over 42 font/size series and
/// on Chrome 152 Windows over 10, and every one fits:
///
/// ```text
/// avg   = max(avg_px, round(avg_px))                         // rounds only UPWARD
/// width = ceil(avg * size + max(0, maxCharWidth_px - avg))   // maxCharWidth = round(bbox / fMaxCharWidth)
/// Arial 13.333px, Linux  (Liberation, xAvg 1187/2048, bbox 3780/2048):  avg 8, max 25 -> 177 content, 185 box
/// Arial 13.333px, Windows (fAvgCharWidth 0 -> adv 'x' 500/1000, fMaxCharWidth 2664.6/1000):  avg 7, max 36 -> 169 content, 177 box
/// ```
///
/// Blink also refuses an average that is more than 1.7 times the width of `0` — "some fonts match
/// avgCharWidth to CJK full-width characters" (layout_text_control.cc `HasValidAvgCharWidth`) — which
/// is a rule over the two numbers the table already has, so it is applied here rather than captured.
/// The px size is the quantized one every other measurement uses: 13.3333px is 13.328125px to a font.
fn char_widths(metrics: &FontMetrics, spec: &FontSpec) -> (f32, Option<f32>) {
    let size = crate::font::used_size(spec.size);
    let zero = metrics.advance('0') * size;
    // Blink's `avg_char_width_`: the font's own average, or the advance of `'x'` when it has none.
    let avg = match metrics.avg_char_width {
        Some(a) if a > 0.0 => a * size,
        _ => metrics.advance('x') * size,
    };
    if avg > 0.0 && !(zero > 0.0 && avg > zero * 1.7) {
        if let Some(max) = metrics.max_char_width {
            return (avg.max(avg.round()), Some((max * size).round()));
        }
    }
    (zero, None)
}

/// `<input>`'s natural size for the text-entry types: `size` average characters wide plus the
/// font's maximum character width less one average (Blink's `TextFieldIntrinsicInlineSize`), and one
/// line tall (`TextFieldIntrinsicBlockSize`: the inner editor's line height).
pub fn text_field(metrics: &FontMetrics, spec: &FontSpec, size: u32) -> Natural {
    let (avg, max) = char_widths(metrics, spec);
    let mut width = avg * size as f32;
    if let Some(max) = max.filter(|&m| m > avg) {
        width += max - avg;
    }
    Natural { width: Some(width.ceil()), height: Some(spec.line_height(metrics)), ratio: None }
}

/// `<textarea>`'s natural size: `cols` average characters wide, `rows` lines tall (Blink's
/// `TextAreaIntrinsicInlineSize` / `TextAreaIntrinsicBlockSize`), plus `bars` — the scrollbar
/// thicknesses Blink adds whether or not anything overflows: the vertical one's to the width when the
/// block axis is `auto` or `scroll`, the horizontal one's to the height only for `overflow-x: scroll`.
/// The caller reads those off the style, whose thickness is the worn theme's.
pub fn textarea(metrics: &FontMetrics, spec: &FontSpec, cols: u32, rows: u32, bars: Size<f32>) -> Natural {
    let (avg, _) = char_widths(metrics, spec);
    let width = (avg * cols as f32).ceil() + bars.width;
    let height = spec.line_height(metrics) * rows as f32 + bars.height;
    Natural { width: Some(width), height: Some(height), ratio: None }
}

/// A drop-down `<select>`'s natural size (Blink's `MenuListIntrinsicInlineSize` /
/// `MenuListIntrinsicBlockSize`): as wide as its widest option's label in the select's own font,
/// and as tall as that font's ascent + descent — the font's `Height()`, which has no line gap in it.
///
/// Blink adds the THEME's internal paddings to both (`PopupInternalPadding*`): measured on Chrome 150
/// Linux across three families and seven sizes, the same inline and block amounts whatever the font.
/// Those are properties of the platform's form-control theme, not of any font, so `padding` is the
/// worn persona's captured theme (`theme.json`), and zero for a control whose `appearance` is `none`.
pub fn menu_list<'a>(
    metrics: &FontMetrics,
    spec: &FontSpec,
    labels: impl Iterator<Item = &'a str>,
    padding: Size<f32>,
) -> Natural {
    let size = crate::font::used_size(spec.size);
    let widest = labels.map(|label| spec.advance_of(metrics, label)).fold(0.0f32, f32::max);
    let height = (metrics.ascent * size).round() + (metrics.descent * size).round();
    Natural { width: Some(widest.ceil() + padding.width), height: Some(height + padding.height), ratio: None }
}

/// A button-type `<input>` (`submit`, `button`, `reset`): its label on one line.
/// A file input's button label and the text beside it while no file is chosen, in the worn browser's UI language
/// (`IDS_FORM_FILE_BUTTON_LABEL`, `IDS_FORM_FILE_NO_FILE_LABEL`): read off the theme's submit label, which is that
/// language's (Chrome 150 under a Korean UI: "파일 선택", "선택된 파일 없음"; English: "Choose File" -- "Choose Files"
/// for a `multiple` one -- and "No file chosen"). Another language takes English's, as its submit label does.
pub fn file_labels(theme: &spec::ThemeProfile, multiple: bool) -> (&'static str, &'static str) {
    match theme.submit_label {
        "제출" => ("파일 선택", "선택된 파일 없음"),
        _ if multiple => ("Choose Files", "No file chosen"),
        _ => ("Choose File", "No file chosen"),
    }
}

/// A file input's natural size (Blink's `LayoutFileUploadControl`): its button -- the label, 6px of padding and a
/// 2px border each side, as tall as the label's line and 1px padding and that border above and below -- 4px of space,
/// and the no-file text; at least 34 zeros wide, rounded up (Chrome 150: 253 x 25 at 13.33px Korean, 379 x 35 at 20px).
pub fn file_input(metrics: &FontMetrics, spec: &FontSpec, button: &str, none: &str, line: f32) -> Natural {
    let button_w = spec.advance_of(metrics, button) + 16.0;
    let width = (button_w + 4.0 + spec.advance_of(metrics, none)).max(34.0 * spec.advance_of(metrics, "0"));
    Natural { width: Some(width.ceil()), height: Some(line + 6.0), ratio: None }
}

/// How a date or time input lays its fields out in the worn browser's UI language: (its fields at their widest --
/// each the widest of its values, a year's being 275760's six digits -- with the text between them; how many fields;
/// the placeholder it shows while empty), keyed off the theme's submit label as [`file_labels`] is. Chrome 150 under
/// a Korean UI: `2026. 10. 23.` (placeholder `연도. 월. 일.`), `오전 10:30`; English: `10/23/2026`, `10:30 AM`.
pub fn datetime_pattern(theme: &spec::ThemeProfile, kind: &str) -> Option<(&'static str, u32, &'static str)> {
    let korean = theme.submit_label == "제출";
    Some(match (kind, korean) {
        ("date", true) => ("275760. 00. 00.", 3, "연도. 월. 일."),
        ("date", false) => ("00/00/275760", 3, "mm/dd/yyyy"),
        ("month", true) => ("275760년 00월", 2, "----년 --월"),
        ("month", false) => ("September 275760", 2, "--------- ----"),
        ("week", true) => ("275760년 00주", 2, "----년 --주"),
        ("week", false) => ("Week 00, 275760", 2, "Week --, ----"),
        ("time", true) => ("오전 00:00", 3, "-- --:--"),
        ("time", false) => ("00:00 AM", 3, "--:-- --"),
        ("datetime-local", true) => ("275760. 00. 00. 오전 00:00", 6, "연도. 월. 일. -- --:--"),
        ("datetime-local", false) => ("00/00/275760, 00:00 AM", 6, "mm/dd/yyyy, --:-- --"),
        _ => return None,
    })
}

/// A date or time input's natural size (Blink's `DateTimeEditElement`): its fields at their widest, each with 1px of
/// padding either side, and its picker icon -- `1em` and 2px of padding each side, a time's `1.05em` with 3px and an
/// 8px margin before it; as tall as the line and the fields' 1px above and below (Chrome 150 under a Korean UI,
/// 13.33px monospace: a date 123.33 x 21, a datetime-local 201.33).
pub fn datetime_input(metrics: &FontMetrics, spec: &FontSpec, widest: &str, fields: u32, time: bool, line: f32) -> Natural {
    let icon = if time { 1.05 * spec.size + 6.0 + 8.0 } else { spec.size + 4.0 };
    let width = spec.advance_of(metrics, widest) + 2.0 * fields as f32 + icon;
    Natural { width: Some(width), height: Some(line + 2.0), ratio: None }
}

pub fn input_button(metrics: &FontMetrics, spec: &FontSpec, label: &str) -> Natural {
    Natural {
        width: Some(spec.advance_of(metrics, label)),
        height: Some(spec.line_height(metrics)),
        ratio: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ratio_table_keeps_a_photo_in_proportion_under_max_width() {
        // A 1200x800 photo, max-width 600.
        assert_eq!(constrain_with_ratio(1200.0, 800.0, 0.0, 600.0, 0.0, f32::INFINITY), (600.0, 400.0));
        // Both maxima violated: the tighter one wins.
        assert_eq!(constrain_with_ratio(1200.0, 800.0, 0.0, 600.0, 0.0, 200.0), (300.0, 200.0));
        // A min grows the other axis with it.
        assert_eq!(constrain_with_ratio(40.0, 30.0, 80.0, f32::INFINITY, 0.0, f32::INFINITY), (80.0, 60.0));
        // Nothing violated.
        assert_eq!(constrain_with_ratio(40.0, 30.0, 0.0, 100.0, 0.0, 100.0), (40.0, 30.0));
    }
}
