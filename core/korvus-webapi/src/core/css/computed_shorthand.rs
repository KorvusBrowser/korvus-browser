//! `getComputedStyle`'s SHORTHANDS, serialized from their longhands' computed values the way Chromium 150's style
//! engine writes them. a bot challenge reads every property of a written frame body's
//! computed style -- the 736 names Chrome's `CSSStyleDeclaration` carries -- and 111 of them (`background`, `font`,
//! `flex`, `border-block`, `transition`, `grid`, `outline`, `columns` ...) read "" here where Chrome answers a value.
//!
//! Each rule follows the Blink function it names (`ComputedStyleUtils`, `shorthands_custom.cc`, `CSSOMUtils`) and
//! works on the strings `getComputedStyle` answers for the longhands, so a longhand answered as Chrome answers it
//! gives its shorthands for free. `Some("")` is Blink's `nullptr`: the longhands are not representable by the
//! shorthand (`border-block` with different start and end sides); `None` means `name` is not a shorthand served here.

/// The computed value of shorthand `name`, from `get`, which answers a longhand's (or a nested shorthand's) computed
/// value as `getPropertyValue` does.
pub(crate) fn serialize(name: &str, get: &mut dyn FnMut(&str) -> String) -> Option<String> {
    let v = match name {
        // `ValuesForShorthandProperty`: every longhand, in the shorthand's order, space-separated
        "flex" => space(get, &["flex-grow", "flex-shrink", "flex-basis"]),
        "flex-flow" => space(get, &["flex-direction", "flex-wrap"]),
        "list-style" => space(get, &["list-style-position", "list-style-image", "list-style-type"]),
        "outline" => space(get, &["outline-color", "outline-style", "outline-width"]),
        "text-emphasis" => space(get, &["text-emphasis-style", "text-emphasis-color"]),
        "-webkit-text-stroke" => space(get, &["-webkit-text-stroke-width", "-webkit-text-stroke-color"]),
        "border-block-start" | "border-block-end" | "border-inline-start" | "border-inline-end" => {
            space(get, &[&format!("{name}-width"), &format!("{name}-style"), &format!("{name}-color")])
        }
        // `ValuesForInlineBlockShorthand` / `ValuesForPlaceShorthand` / `ValuesForInterestDelayShorthand` /
        // `OverscrollBehavior` / `BorderSpacing`: a pair, one value when both agree
        "margin-block" | "margin-inline" | "padding-block" | "padding-inline" | "inset-block" | "inset-inline" | "scroll-margin-block"
        | "scroll-margin-inline" | "scroll-padding-block" | "scroll-padding-inline" | "interest-delay" => {
            pair(get, &format!("{name}-start"), &format!("{name}-end"))
        }
        "border-block-color" | "border-block-style" | "border-block-width" | "border-inline-color" | "border-inline-style"
        | "border-inline-width" => {
            let (axis, part) = name.rsplit_once('-')?;
            pair(get, &format!("{axis}-start-{part}"), &format!("{axis}-end-{part}"))
        }
        "place-content" => pair(get, "align-content", "justify-content"),
        "place-items" => pair(get, "align-items", "justify-items"),
        "place-self" => pair(get, "align-self", "justify-self"),
        "overscroll-behavior" => pair(get, "overscroll-behavior-x", "overscroll-behavior-y"),
        "border-spacing" => pair(get, "-webkit-border-horizontal-spacing", "-webkit-border-vertical-spacing"),
        // `BorderBlock` / `BorderInline`: the start side when the end side is the same, else nothing
        "border-block" | "border-inline" => {
            let start = serialize(&format!("{name}-start"), get)?;
            let end = serialize(&format!("{name}-end"), get)?;
            if start == end { start } else { String::new() }
        }
        // `Marker`: the one resource all three name, else nothing
        "marker" => same(get, &["marker-start", "marker-mid", "marker-end"]),
        // `ValueForCornerShapeShorthand` (the four corners as box sides) and `ValueForCornerShapeEdgeShorthand`
        "corner-shape" => sides(get, ["corner-top-left-shape", "corner-top-right-shape", "corner-bottom-right-shape", "corner-bottom-left-shape"]),
        "corner-top-shape" => pair(get, "corner-top-left-shape", "corner-top-right-shape"),
        "corner-right-shape" => pair(get, "corner-top-right-shape", "corner-bottom-right-shape"),
        "corner-bottom-shape" => pair(get, "corner-bottom-left-shape", "corner-bottom-right-shape"),
        "corner-left-shape" => pair(get, "corner-top-left-shape", "corner-bottom-left-shape"),
        "corner-block-start-shape" => pair(get, "corner-start-start-shape", "corner-start-end-shape"),
        "corner-block-end-shape" => pair(get, "corner-end-start-shape", "corner-end-end-shape"),
        "corner-inline-start-shape" => pair(get, "corner-start-start-shape", "corner-end-start-shape"),
        "corner-inline-end-shape" => pair(get, "corner-start-end-shape", "corner-end-end-shape"),
        // `ValueForGapDecorationRuleShorthand`: each rule's width, style and colour, the initial ones left out
        "column-rule" | "row-rule" => {
            let axis = name.trim_end_matches("-rule");
            gap_rules(&get(&format!("{axis}-rule-width")), &get(&format!("{axis}-rule-style")), &get(&format!("{axis}-rule-color")))
        }
        // the bidirectional `rule*` shorthands (`ValuesForBidirectionalGapRule*`): the column side when the row
        // side is the same, else nothing
        "rule" | "rule-inset" | "rule-inset-cap" | "rule-inset-junction" | "rule-inset-start" | "rule-inset-end" => {
            let part = name.trim_start_matches("rule");
            let col = serialize(&format!("column-rule{part}"), get)?;
            let row = serialize(&format!("row-rule{part}"), get)?;
            if col == row { col } else { String::new() }
        }
        "rule-color" | "rule-style" | "rule-width" | "rule-break" | "rule-visibility-items" => {
            let part = name.trim_start_matches("rule-");
            same(get, &[&format!("column-rule-{part}"), &format!("row-rule-{part}")])
        }
        // `ValuesForGapDecorationRuleInsetShorthand`: one value when all four agree, else `caps / junctions`
        "column-rule-inset" | "row-rule-inset" => {
            let axis = name.trim_end_matches("-rule-inset");
            let v: Vec<String> = ["cap-start", "cap-end", "junction-start", "junction-end"].iter().map(|p| get(&format!("{axis}-rule-inset-{p}"))).collect();
            if v.iter().any(|x| x.is_empty()) {
                String::new()
            } else if v.iter().all(|x| *x == v[0]) {
                v[0].clone()
            } else {
                format!("{} {} / {} {}", v[0], v[1], v[2], v[3])
            }
        }
        // `...InsetCapJunctionShorthand`: start, and end when it differs; `...InsetStartEndShorthand`: the cap's
        // value when the junction's is the same, else nothing
        "column-rule-inset-cap" | "row-rule-inset-cap" | "column-rule-inset-junction" | "row-rule-inset-junction" => {
            pair(get, &format!("{name}-start"), &format!("{name}-end"))
        }
        "column-rule-inset-start" | "row-rule-inset-start" | "column-rule-inset-end" | "row-rule-inset-end" => {
            let (base, edge) = name.rsplit_once('-')?;
            same(get, &[&format!("{base}-cap-{edge}"), &format!("{base}-junction-{edge}")])
        }
        // `Columns`: `auto` for an auto width and count (and height), else the ones that are not, the height after
        // a slash
        "columns" => {
            let (w, c, h) = (get("column-width"), get("column-count"), get("column-height"));
            if w.is_empty() || c.is_empty() {
                String::new()
            } else {
                let pre = match (w == "auto", c == "auto") {
                    (true, true) => "auto".to_string(),
                    (true, false) => c,
                    (false, true) => w,
                    (false, false) => format!("{w} {c}"),
                };
                if h.is_empty() || h == "auto" { pre } else { format!("{pre} / {h}") }
            }
        }
        // `ValuesForContainerShorthand`: the name, then the type unless `normal`
        "container" => {
            let (n, t) = (get("container-name"), get("container-type"));
            if n.is_empty() || t.is_empty() { String::new() } else if t == "normal" { n } else { format!("{n} / {t}") }
        }
        // `TextWrap`: the mode for an initial style, the style for an initial mode, else both
        "text-wrap" => {
            let (mode, style) = (get("text-wrap-mode"), get("text-wrap-style"));
            if mode.is_empty() || style.is_empty() {
                String::new()
            } else if style == "auto" {
                mode
            } else if mode == "wrap" {
                style
            } else {
                format!("{mode} {style}")
            }
        }
        // `TextBox`: `normal` or the trim for an `auto` edge; the edge alone under `trim-both`; else both
        "text-box" => {
            let (trim, edge) = (get("text-box-trim"), get("text-box-edge"));
            if trim.is_empty() || edge.is_empty() {
                String::new()
            } else if edge == "auto" {
                if trim == "none" { "normal".to_string() } else { trim }
            } else if trim == "trim-both" {
                edge
            } else {
                format!("{trim} {edge}")
            }
        }
        // `ValuesForFontSynthesisProperty`: the parts that are `auto`, by their names, or `none`
        "font-synthesis" => {
            let parts = [("font-synthesis-weight", "weight"), ("font-synthesis-style", "style"), ("font-synthesis-small-caps", "small-caps")];
            let on: Vec<&str> = parts.iter().filter(|(l, _)| get(l) == "auto").map(|(_, n)| *n).collect();
            if on.is_empty() { "none".to_string() } else { on.join(" ") }
        }
        // the legacy break shorthands (`ValueForPageBreakBetween` / `...Inside`, `ValueForWebkitColumnBreak...`)
        "page-break-after" | "page-break-before" => match get(&format!("break-{}", name.trim_start_matches("page-break-"))).as_str() {
            "avoid-column" | "column" | "recto" | "verso" | "avoid-page" | "" => String::new(),
            "page" => "always".to_string(),
            v => v.to_string(),
        },
        "page-break-inside" => match get("break-inside").as_str() {
            "avoid-column" | "" => String::new(),
            "avoid-page" => "avoid".to_string(),
            v => v.to_string(),
        },
        "-webkit-column-break-after" | "-webkit-column-break-before" => {
            match get(&format!("break-{}", name.trim_start_matches("-webkit-column-break-"))).as_str() {
                "avoid-page" | "left" | "page" | "recto" | "right" | "verso" | "" => String::new(),
                "column" => "always".to_string(),
                "avoid-column" => "avoid".to_string(),
                v => v.to_string(),
            }
        }
        "-webkit-column-break-inside" => match get("break-inside").as_str() {
            "avoid-page" | "" => String::new(),
            "avoid-column" => "avoid".to_string(),
            v => v.to_string(),
        },
        // `PositionTry`: the order unless `normal`, then the fallbacks
        "position-try" => {
            let (order, fallbacks) = (get("position-try-order"), get("position-try-fallbacks"));
            if order.is_empty() || fallbacks.is_empty() { String::new() } else if order == "normal" { fallbacks } else { format!("{order} {fallbacks}") }
        }
        // `ValueForOffset`: the position unless `normal`, path, distance, rotate, then the anchor after a slash
        // unless `auto`
        "offset" => {
            let (pos, path, dist, rot, anchor) = (get("offset-position"), get("offset-path"), get("offset-distance"), get("offset-rotate"), get("offset-anchor"));
            if [&pos, &path, &dist, &rot, &anchor].iter().any(|v| v.is_empty()) {
                String::new()
            } else {
                let mut out = Vec::new();
                if pos != "normal" {
                    out.push(pos);
                }
                out.extend([path, dist, rot]);
                let head = out.join(" ");
                if anchor == "auto" { head } else { format!("{head} / {anchor}") }
            }
        }
        // `ValueForBorderRadiusShorthand`: the horizontal radii as box sides, the vertical after a slash when they
        // differ
        "border-radius" => {
            let corners: Vec<(String, String)> = ["border-top-left-radius", "border-top-right-radius", "border-bottom-right-radius", "border-bottom-left-radius"]
                .iter()
                .map(|l| {
                    let t = tokens(&get(l));
                    let w = t.first().cloned().unwrap_or_default();
                    let h = t.get(1).cloned().unwrap_or_else(|| w.clone());
                    (w, h)
                })
                .collect();
            if corners.iter().any(|(w, _)| w.is_empty()) {
                String::new()
            } else {
                let horizontal = sides_of([&corners[0].0, &corners[1].0, &corners[2].0, &corners[3].0]);
                let vertical = sides_of([&corners[0].1, &corners[1].1, &corners[2].1, &corners[3].1]);
                if horizontal == vertical { horizontal } else { format!("{horizontal} / {vertical}") }
            }
        }
        // `ValueForNinePieceImage`: `none` without an image, else source, slice / width / outset, repeat
        "border-image" => {
            let source = get("border-image-source");
            if source.is_empty() || source == "none" {
                if source.is_empty() { String::new() } else { "none".to_string() }
            } else {
                let parts = [get("border-image-slice"), get("border-image-width"), get("border-image-outset"), get("border-image-repeat")];
                if parts.iter().any(|p| p.is_empty()) {
                    String::new()
                } else {
                    format!("{source} {} / {} / {} {}", parts[0], parts[1], parts[2], parts[3])
                }
            }
        }
        // `ValueForFont`
        "font" => font(get),
        // `ValuesForGridShorthand`: the six longhands slash-separated, trailing `none`s left out
        "grid" => {
            let v: Vec<String> = ["grid-template-rows", "grid-template-columns", "grid-template-areas", "grid-auto-flow", "grid-auto-rows", "grid-auto-columns"]
                .iter()
                .map(|l| get(l))
                .collect();
            if v.iter().any(|x| x.is_empty()) {
                String::new()
            } else {
                let mut last = v.len();
                while last > 1 && v[last - 1] == "none" {
                    last -= 1;
                }
                v[..last].join(" / ")
            }
        }
        "grid-template" => grid_template(&get("grid-template-rows"), &get("grid-template-columns"), &get("grid-template-areas")),
        "background" => background(get),
        "background-position" => {
            let (x, y) = (items(&get("background-position-x")), items(&get("background-position-y")));
            if x.iter().chain(y.iter()).any(|v| v.is_empty()) {
                String::new()
            } else {
                (0..x.len().max(y.len())).map(|i| format!("{} {}", nth(&x, i), nth(&y, i))).collect::<Vec<_>>().join(", ")
            }
        }
        "mask-position" => {
            let (x, y) = (items(&get("-webkit-mask-position-x")), items(&get("-webkit-mask-position-y")));
            if x.iter().chain(y.iter()).any(|v| v.is_empty()) {
                String::new()
            } else {
                (0..x.len().max(y.len())).map(|i| format!("{} {}", nth(&x, i), nth(&y, i))).collect::<Vec<_>>().join(", ")
            }
        }
        "mask" => mask(get),
        "transition" => transition(get),
        "animation" => animation(get),
        // `AnimationRangeCSSValueFromComputedStyle`
        "animation-range" => ranges(&get("animation-range-start"), &get("animation-range-end")),
        "timeline-trigger-activation-range" => ranges(&get("timeline-trigger-activation-range-start"), &get("timeline-trigger-activation-range-end")),
        // `TimelineTriggerExitRangeCSSValueFromComputedStyle`
        "timeline-trigger-active-range" => exit_ranges(&get("timeline-trigger-active-range-start"), &get("timeline-trigger-active-range-end")),
        "timeline-trigger" => timeline_trigger(get),
        // `CSSValueForTimelineShorthand`
        "scroll-timeline" => timeline(&get("scroll-timeline-name"), &get("scroll-timeline-axis"), None),
        "view-timeline" => timeline(&get("view-timeline-name"), &get("view-timeline-axis"), Some(&get("view-timeline-inset"))),
        _ => return None,
    };
    Some(v)
}

/// The longhands' values, space-separated; nothing when one has none.
fn space(get: &mut dyn FnMut(&str) -> String, longhands: &[&str]) -> String {
    let v: Vec<String> = longhands.iter().map(|l| get(l)).collect();
    if v.iter().any(|x| x.is_empty()) { String::new() } else { v.join(" ") }
}

/// `CSSValuePair::kDropIdenticalValues`.
fn pair(get: &mut dyn FnMut(&str) -> String, a: &str, b: &str) -> String {
    let (a, b) = (get(a), get(b));
    if a.is_empty() || b.is_empty() {
        String::new()
    } else if a == b {
        a
    } else {
        format!("{a} {b}")
    }
}

/// The value every longhand shares, or nothing.
fn same(get: &mut dyn FnMut(&str) -> String, longhands: &[&str]) -> String {
    let v: Vec<String> = longhands.iter().map(|l| get(l)).collect();
    if v.iter().any(|x| x.is_empty() || *x != v[0]) { String::new() } else { v[0].clone() }
}

/// `ValuesForSidesShorthand` over four longhands, clockwise from the top.
fn sides(get: &mut dyn FnMut(&str) -> String, longhands: [&str; 4]) -> String {
    let v: Vec<String> = longhands.iter().map(|l| get(l)).collect();
    if v.iter().any(|x| x.is_empty()) { String::new() } else { sides_of([&v[0], &v[1], &v[2], &v[3]]) }
}

/// The shortest box-sides form: the last is left out when it repeats the second, the third when it also repeats
/// the first, the second when it also repeats the first.
fn sides_of([t, r, b, l]: [&str; 4]) -> String {
    let show_left = r != l;
    let show_bottom = show_left || b != t;
    let show_right = show_bottom || r != t;
    let mut out = vec![t];
    if show_right {
        out.push(r);
    }
    if show_bottom {
        out.push(b);
    }
    if show_left {
        out.push(l);
    }
    out.join(" ")
}

/// A computed list's items, split at its top-level commas.
pub(crate) fn items(v: &str) -> Vec<String> {
    split_top(v, |c| c == ',').into_iter().map(|s| s.trim().to_string()).collect()
}

/// A value's top-level space-separated components (`minmax(1px, 2fr)`, `[a b]` and `"x y"` whole).
fn tokens(v: &str) -> Vec<String> {
    split_top(v, |c| c.is_whitespace()).into_iter().filter(|s| !s.is_empty()).collect()
}

fn split_top(v: &str, at: impl Fn(char) -> bool) -> Vec<String> {
    let mut out = Vec::new();
    let (mut depth, mut quote, mut cur) = (0i32, None::<char>, String::new());
    for ch in v.chars() {
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                }
            }
            None => match ch {
                '"' | '\'' => quote = Some(ch),
                '(' | '[' => depth += 1,
                ')' | ']' => depth -= 1,
                c if depth == 0 && at(c) => {
                    out.push(std::mem::take(&mut cur));
                    continue;
                }
                _ => {}
            },
        }
        cur.push(ch);
    }
    out.push(cur);
    out
}

/// Item `i` of a computed list, the list repeating as a coordinated list does.
fn nth(list: &[String], i: usize) -> &str {
    if list.is_empty() { "" } else { &list[i % list.len()] }
}

/// `GetValueListForGapRule` per rule: the width unless `medium`, the style unless `none`, the colour unless
/// `currentcolor` (`medium` when all three are left out); nothing when the lists do not line up. A `repeat()` is
/// rebuilt from its three halves (`repeat(2, 3px solid red)`).
fn gap_rules(widths: &str, styles: &str, colors: &str) -> String {
    let (w, s, c) = (items(widths), items(styles), items(colors));
    if w.len() != s.len() || w.len() != c.len() || w.iter().chain(&s).chain(&c).any(|x| x.is_empty()) {
        return String::new();
    }
    let one = |w: &str, s: &str, c: &str| -> String {
        let mut out = Vec::new();
        if w != "medium" {
            out.push(w);
        }
        if s != "none" {
            out.push(s);
        }
        if c != "currentcolor" {
            out.push(c);
        }
        if out.is_empty() { "medium".to_string() } else { out.join(" ") }
    };
    let repeat = |v: &str| -> Option<(String, Vec<String>)> {
        let inner = v.strip_prefix("repeat(")?.strip_suffix(')')?;
        let mut parts = items(inner).into_iter();
        let count = parts.next()?;
        Some((count, parts.collect()))
    };
    let mut out = Vec::new();
    for i in 0..w.len() {
        match (repeat(&w[i]), repeat(&s[i]), repeat(&c[i])) {
            (Some((n, wv)), Some((ns, sv)), Some((nc, cv))) => {
                if n != ns || n != nc || wv.len() != sv.len() || wv.len() != cv.len() {
                    return String::new();
                }
                let rules: Vec<String> = (0..wv.len()).map(|j| one(&wv[j], &sv[j], &cv[j])).collect();
                out.push(format!("repeat({n}, {})", rules.join(", ")));
            }
            (None, None, None) => out.push(one(&w[i], &s[i], &c[i])),
            _ => return String::new(),
        }
    }
    out.join(", ")
}

/// `ValuesForBackgroundShorthand`: each layer's image, repeat, attachment, position / size, origin, clip -- the
/// colour first in the last layer.
fn background(get: &mut dyn FnMut(&str) -> String) -> String {
    let images = items(&get("background-image"));
    let lists: Vec<Vec<String>> = ["background-repeat", "background-attachment", "background-position-x", "background-position-y", "background-size", "background-origin", "background-clip"]
        .iter()
        .map(|l| items(&get(l)))
        .collect();
    let color = get("background-color");
    if color.is_empty() || images.iter().any(|v| v.is_empty()) || lists.iter().any(|l| l.iter().any(|v| v.is_empty())) {
        return String::new();
    }
    let n = images.len();
    (0..n)
        .map(|i| {
            let mut before = Vec::new();
            if i + 1 == n {
                before.push(color.clone());
            }
            before.extend([images[i].clone(), nth(&lists[0], i).to_string(), nth(&lists[1], i).to_string()]);
            before.push(format!("{} {}", nth(&lists[2], i), nth(&lists[3], i)));
            format!("{} / {} {} {}", before.join(" "), nth(&lists[4], i), nth(&lists[5], i), nth(&lists[6], i))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `ValuesForMaskShorthand`: each layer's image, position [/ size], repeat, origin and clip, composite and mode,
/// each left out while it is its initial value; `none` for a layer with nothing left.
fn mask(get: &mut dyn FnMut(&str) -> String) -> String {
    let images = items(&get("mask-image"));
    let lists: Vec<Vec<String>> = ["-webkit-mask-position-x", "-webkit-mask-position-y", "mask-size", "mask-repeat", "mask-origin", "mask-clip", "mask-composite", "mask-mode"]
        .iter()
        .map(|l| items(&get(l)))
        .collect();
    if images.iter().any(|v| v.is_empty()) || lists.iter().any(|l| l.iter().any(|v| v.is_empty())) {
        return String::new();
    }
    (0..images.len())
        .map(|i| {
            let mut out = Vec::new();
            if images[i] != "none" {
                out.push(images[i].clone());
            }
            let (x, y, size) = (nth(&lists[0], i), nth(&lists[1], i), nth(&lists[2], i));
            if x != "0%" || y != "0%" || size != "auto" {
                out.push(if size != "auto" { format!("{x} {y} / {size}") } else { format!("{x} {y}") });
            }
            let repeat = nth(&lists[3], i);
            if repeat != "repeat" {
                out.push(repeat.to_string());
            }
            // `AppendValuesForMaskClipAndOrigin`
            let (origin, clip) = (nth(&lists[4], i), nth(&lists[5], i));
            if origin == clip {
                if origin != "border-box" {
                    out.push(origin.to_string());
                }
            } else if origin == "border-box" && clip == "no-clip" {
                out.push(clip.to_string());
            } else {
                out.push(origin.to_string());
                out.push(clip.to_string());
            }
            let composite = nth(&lists[6], i);
            if composite != "add" {
                out.push(composite.to_string());
            }
            let mode = nth(&lists[7], i);
            if mode != "match-source" {
                out.push(mode.to_string());
            }
            if out.is_empty() { "none".to_string() } else { out.join(" ") }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `Transition::CSSValueFromComputedStyleInternal`: per transition the property unless `all`, the duration when it
/// or the delay is not `0s`, the timing function unless `ease`, the delay unless `0s`, the behavior unless
/// `normal` -- `all` when nothing is left.
fn transition(get: &mut dyn FnMut(&str) -> String) -> String {
    let props = items(&get("transition-property"));
    let lists: Vec<Vec<String>> = ["transition-duration", "transition-timing-function", "transition-delay", "transition-behavior"].iter().map(|l| items(&get(l))).collect();
    if props.iter().any(|v| v.is_empty()) || lists.iter().any(|l| l.iter().any(|v| v.is_empty())) {
        return String::new();
    }
    (0..props.len())
        .map(|i| {
            let mut out = Vec::new();
            if props[i] != "all" {
                out.push(props[i].clone());
            }
            let (duration, timing, delay, behavior) = (nth(&lists[0], i), nth(&lists[1], i), nth(&lists[2], i), nth(&lists[3], i));
            let shows_delay = delay != "0s";
            if shows_delay || duration != "0s" {
                out.push(duration.to_string());
            }
            if timing != "ease" {
                out.push(timing.to_string());
            }
            if shows_delay {
                out.push(delay.to_string());
            }
            if behavior != "normal" {
                out.push(behavior.to_string());
            }
            if out.is_empty() { "all".to_string() } else { out.join(" ") }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `CSSValueFromComputedAnimation`: nothing when a timeline or a range is not its single initial value (the
/// shorthand resets them and cannot say them); per animation the duration unless initial, the timing function,
/// the delay (with the duration before it), the iteration count, direction, fill mode and play state unless
/// initial -- each also when the name would read as that keyword -- then the name unless `none`; `none` when
/// nothing is left.
fn animation(get: &mut dyn FnMut(&str) -> String) -> String {
    for (l, initial) in [("animation-timeline", "auto"), ("animation-range-start", "normal"), ("animation-range-end", "normal")] {
        if get(l) != initial {
            return String::new();
        }
    }
    let names = items(&get("animation-name"));
    let keys = ["animation-duration", "animation-timing-function", "animation-delay", "animation-iteration-count", "animation-direction", "animation-fill-mode", "animation-play-state"];
    let lists: Vec<Vec<String>> = keys.iter().map(|l| items(&get(l))).collect();
    if names.iter().any(|v| v.is_empty()) || lists.iter().any(|l| l.iter().any(|v| v.is_empty())) {
        return String::new();
    }
    // `CheckAnimationNameConflicts`: a name that is also a keyword of one of these is preceded by that keyword
    const TIMING: &[&str] = &["linear", "ease", "ease-in", "ease-out", "ease-in-out", "step-start", "step-end"];
    const DIRECTION: &[&str] = &["normal", "reverse", "alternate", "alternate-reverse"];
    const FILL: &[&str] = &["none", "forwards", "backwards", "both"];
    const PLAY: &[&str] = &["running", "paused"];
    (0..names.len())
        .map(|i| {
            let name = names[i].as_str();
            let conflict = |set: &[&str]| set.contains(&name);
            let v: Vec<&str> = (0..keys.len()).map(|k| nth(&lists[k], i)).collect();
            let (duration, timing, delay, count, direction, fill, play) = (v[0], v[1], v[2], v[3], v[4], v[5], v[6]);
            let mut out = Vec::new();
            if duration != "0s" {
                out.push(duration);
            }
            if timing != "ease" || conflict(TIMING) {
                out.push(timing);
            }
            if delay != "0s" {
                if duration == "0s" {
                    out.push("0s");
                }
                out.push(delay);
            }
            if count != "1" {
                out.push(count);
            }
            if direction != "normal" || conflict(DIRECTION) {
                out.push(direction);
            }
            if fill != "none" || conflict(FILL) {
                out.push(fill);
            }
            if play != "running" || conflict(PLAY) {
                out.push(play);
            }
            if name != "none" {
                out.push(name);
            }
            if out.is_empty() { "none".to_string() } else { out.join(" ") }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// A timeline range offset as `(named range, offset)`: `normal` is no range at `default`, a bare range name is
/// that range at `default`, a bare offset is no range.
fn range_offset<'a>(v: &'a str, default: &'a str) -> (&'a str, &'a str) {
    if v == "normal" {
        return ("none", default);
    }
    match v.split_once(' ') {
        Some((name, offset)) => (name, offset),
        None if v.starts_with(|c: char| c.is_ascii_digit() || matches!(c, '-' | '+' | '.')) || v.starts_with("calc(") => ("none", v),
        None => (v, default),
    }
}

/// `SingleAnimationRangeCSSValueFromComputedStyle`: the start, and the end unless it is the start's own range at
/// 100% ("name X name 100%" contracts to "name X").
fn one_range(start: &str, end: &str) -> String {
    let (name, _) = range_offset(start, "0%");
    if range_offset(end, "100%") == (name, "100%") { start.to_string() } else { format!("{start} {end}") }
}

fn ranges(starts: &str, ends: &str) -> String {
    let (s, e) = (items(starts), items(ends));
    if s.len() != e.len() || s.iter().chain(&e).any(|v| v.is_empty()) {
        return String::new();
    }
    (0..s.len()).map(|i| one_range(&s[i], &e[i])).collect::<Vec<_>>().join(", ")
}

/// `SingleTimelineTriggerExitRangeCSSValueFromComputedStyle`: an `auto` end is left out, a `normal` one kept (so
/// `normal normal` stays two words); otherwise as an animation range.
fn exit_range(start: &str, end: &str) -> String {
    match end {
        "auto" => start.to_string(),
        "normal" => format!("{start} normal"),
        _ => one_range(start, end),
    }
}

fn exit_ranges(starts: &str, ends: &str) -> String {
    let (s, e) = (items(starts), items(ends));
    if s.len() != e.len() || s.iter().chain(&e).any(|v| v.is_empty()) {
        return String::new();
    }
    (0..s.len()).map(|i| exit_range(&s[i], &e[i])).collect::<Vec<_>>().join(", ")
}

/// `TimelineTrigger::CSSValueFromComputedStyleInternal`: per trigger its name unless `none`, its source unless
/// `auto`, its activation range unless `normal`, then its active range after a slash unless `auto`; `none` for a
/// trigger with nothing to say.
fn timeline_trigger(get: &mut dyn FnMut(&str) -> String) -> String {
    let names = items(&get("timeline-trigger-name"));
    let lists: Vec<Vec<String>> = [
        "timeline-trigger-source",
        "timeline-trigger-activation-range-start",
        "timeline-trigger-activation-range-end",
        "timeline-trigger-active-range-start",
        "timeline-trigger-active-range-end",
    ]
    .iter()
    .map(|l| items(&get(l)))
    .collect();
    if names.iter().any(|v| v.is_empty()) || lists.iter().any(|l| l.iter().any(|v| v.is_empty())) {
        return String::new();
    }
    (0..names.len())
        .map(|i| {
            let mut before = Vec::new();
            if names[i] != "none" {
                before.push(names[i].clone());
            }
            let source = nth(&lists[0], i);
            if source != "auto" {
                before.push(source.to_string());
            }
            let enter = one_range(nth(&lists[1], i), nth(&lists[2], i));
            if enter != "normal" {
                before.push(enter);
            }
            let exit = exit_range(nth(&lists[3], i), nth(&lists[4], i));
            let mut parts = vec![before.join(" ")];
            if exit != "auto" {
                parts.push(exit);
            }
            let text = parts.join(" / ");
            if text.is_empty() { "none".to_string() } else { text }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `CSSValueForTimelineShorthand`: per timeline its name, its axis unless `block`, its inset unless `auto`.
fn timeline(names: &str, axes: &str, insets: Option<&str>) -> String {
    let (n, a) = (items(names), items(axes));
    let ins = insets.map(items).unwrap_or_default();
    if n.iter().chain(&a).chain(&ins).any(|v| v.is_empty()) {
        return String::new();
    }
    (0..n.len())
        .map(|i| {
            let mut out = vec![n[i].as_str()];
            let axis = nth(&a, i);
            if axis != "block" {
                out.push(axis);
            }
            if !ins.is_empty() {
                let inset = nth(&ins, i);
                if inset != "auto" {
                    out.push(inset);
                }
            }
            out.join(" ")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `ValueForFont`: style, caps, weight, stretch (each unless `normal`), size [/ line-height], family -- and
/// nothing when a longhand the shorthand cannot say is not initial.
fn font(get: &mut dyn FnMut(&str) -> String) -> String {
    for (l, initial) in [
        ("font-variant-ligatures", "normal"),
        ("font-variant-numeric", "normal"),
        ("font-variant-east-asian", "normal"),
        ("font-feature-settings", "normal"),
        ("font-variation-settings", "normal"),
        ("font-variant-alternates", "normal"),
        ("font-language-override", "normal"),
        ("font-kerning", "auto"),
        ("font-optical-sizing", "auto"),
        ("font-size-adjust", "none"),
        ("font-variant-position", "normal"),
        ("font-variant-emoji", "normal"),
    ] {
        if get(l) != initial {
            return String::new();
        }
    }
    // `ValueForFontStretchAsKeyword`
    let stretch = match get("font-stretch").as_str() {
        "50%" | "ultra-condensed" => "ultra-condensed",
        "62.5%" | "extra-condensed" => "extra-condensed",
        "75%" | "condensed" => "condensed",
        "87.5%" | "semi-condensed" => "semi-condensed",
        "100%" | "normal" => "normal",
        "112.5%" | "semi-expanded" => "semi-expanded",
        "125%" | "expanded" => "expanded",
        "150%" | "extra-expanded" => "extra-expanded",
        "200%" | "ultra-expanded" => "ultra-expanded",
        _ => return String::new(),
    };
    let caps = get("font-variant-caps");
    if caps != "normal" && caps != "small-caps" {
        return String::new();
    }
    let (style, weight, size, line_height, family) = (get("font-style"), get("font-weight"), get("font-size"), get("line-height"), get("font-family"));
    if [&style, &weight, &size, &line_height, &family].iter().any(|v| v.is_empty()) {
        return String::new();
    }
    let mut out = Vec::new();
    if style != "normal" {
        out.push(style);
    }
    if caps != "normal" {
        out.push(caps);
    }
    if weight != "400" {
        out.push(weight);
    }
    if stretch != "normal" {
        out.push(stretch.to_string());
    }
    out.push(if line_height == "normal" { size } else { format!("{size} / {line_height}") });
    out.push(family);
    out.join(" ")
}

/// `CSSOMUtils::ComputedValueForGridTemplateShorthand`.
fn grid_template(rows: &str, cols: &str, areas: &str) -> String {
    if rows.is_empty() || cols.is_empty() || areas.is_empty() {
        return String::new();
    }
    let (rows_none, cols_none, areas_none) = (rows == "none", cols == "none", areas == "none");
    if rows_none && cols_none && areas_none {
        return rows.to_string();
    }
    // areas need rows to line up with
    if !areas_none && rows_none {
        return String::new();
    }
    if rows_none || areas_none {
        return format!("{rows} / {cols}");
    }
    // the ASCII-art form has no `repeat()`
    if rows.contains("repeat(") || cols.contains("repeat(") {
        return String::new();
    }
    let area_rows: Vec<&str> = areas.split('"').skip(1).step_by(2).collect();
    let mut out = Vec::new();
    let mut row = 0;
    for t in tokens(rows) {
        if t.starts_with('[') {
            out.push(t);
            continue;
        }
        let Some(text) = area_rows.get(row) else { return String::new() };
        out.push(format!("\"{text}\""));
        row += 1;
        if t != "auto" {
            out.push(t);
        }
    }
    if row != area_rows.len() {
        return String::new();
    }
    let head = out.join(" ");
    if cols_none { head } else { format!("{head} / {cols}") }
}

/// A position component as Blink's `CreatePositionListForLayer` writes a layer's: a keyword as the percentage it
/// is, an offset from the right or bottom edge as `100%` less it (`right 5px` is `calc(100% - 5px)`, `right 10%`
/// is `90%`).
pub(crate) fn position_component(v: &str) -> String {
    let t = tokens(v);
    let pct = |s: &str| s.strip_suffix('%').and_then(|n| n.parse::<f64>().ok());
    match t.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["left" | "top"] => "0%".to_string(),
        ["center"] => "50%".to_string(),
        ["right" | "bottom"] => "100%".to_string(),
        ["left" | "top", off] => off.to_string(),
        ["right" | "bottom", off] => match pct(off) {
            Some(p) => format!("{}%", page::value::serialize_number_f64(100.0 - p)),
            None => format!("calc(100% - {off})"),
        },
        _ => v.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(values: &[(&str, &str)], name: &str) -> String {
        let mut get = |l: &str| values.iter().find(|(n, _)| *n == l).map(|(_, v)| v.to_string()).unwrap_or_default();
        serialize(name, &mut get).unwrap_or_else(|| panic!("{name} is a shorthand"))
    }

    #[test]
    fn box_sides_and_pairs_take_their_shortest_form() {
        fn radii(tl: &str) -> [(&str, &str); 4] {
            [("border-top-left-radius", tl), ("border-top-right-radius", "0px"), ("border-bottom-right-radius", "0px"), ("border-bottom-left-radius", "0px")]
        }
        assert_eq!(with(&radii("3px 6px"), "border-radius"), "3px 0px 0px / 6px 0px 0px");
        assert_eq!(with(&radii("0px"), "border-radius"), "0px");
        assert_eq!(with(&[("align-content", "space-between"), ("justify-content", "normal")], "place-content"), "space-between normal");
        assert_eq!(with(&[("overscroll-behavior-x", "contain"), ("overscroll-behavior-y", "contain")], "overscroll-behavior"), "contain");
    }

    #[test]
    fn list_shorthands_leave_out_their_initial_parts() {
        let t = [("transition-property", "all, color"), ("transition-duration", "1s, 2s"), ("transition-timing-function", "ease, linear"), ("transition-delay", "0s"), ("transition-behavior", "normal")];
        assert_eq!(with(&t, "transition"), "1s, color 2s linear");
        let a = [
            ("animation-name", "none"),
            ("animation-duration", "0s"),
            ("animation-timing-function", "ease"),
            ("animation-delay", "0s"),
            ("animation-iteration-count", "1"),
            ("animation-direction", "normal"),
            ("animation-fill-mode", "none"),
            ("animation-play-state", "running"),
            ("animation-timeline", "auto"),
            ("animation-range-start", "normal"),
            ("animation-range-end", "normal"),
        ];
        assert_eq!(with(&a, "animation"), "none");
        assert_eq!(with(&[("animation-range-start", "entry 10%"), ("animation-range-end", "exit 90%")], "animation-range"), "entry 10% exit 90%");
        assert_eq!(with(&[("animation-range-start", "contain"), ("animation-range-end", "contain")], "animation-range"), "contain");
        assert_eq!(with(&[("animation-range-start", "normal"), ("animation-range-end", "normal")], "animation-range"), "normal");
    }

    #[test]
    fn positions_read_as_percentages_from_the_left_and_top() {
        assert_eq!(position_component("right 5px"), "calc(100% - 5px)");
        assert_eq!(position_component("right 10%"), "90%");
        assert_eq!(position_component("bottom"), "100%");
        assert_eq!(position_component("center"), "50%");
        assert_eq!(position_component("10px"), "10px");
    }

    #[test]
    fn grid_template_writes_its_ascii_art() {
        assert_eq!(grid_template("20px 30px", "1fr 1fr", "\"a a\" \"b c\""), "\"a a\" 20px \"b c\" 30px / 1fr 1fr");
        assert_eq!(grid_template("none", "none", "none"), "none");
        assert_eq!(grid_template("100px", "1fr 1fr", "none"), "100px / 1fr 1fr");
        assert_eq!(grid_template("repeat(2, 10px)", "none", "\"a\" \"b\""), "");
    }
}
