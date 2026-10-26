//! A DECLARED value in the form Chrome serializes it: what `el.style.getPropertyValue(p)` reads back
//! after `el.style.setProperty(p, v)`, over the token-level canonical form (S31, whitespace and numbers) that the
//! caller has already produced.
//!
//! Measured on stock Chrome 150.0.7871.46 (2026-09-30, `tools/realweb/seed/declcanon`), each rule below with its
//! case. A value is parsed into COMPONENTS (tokens, and functions over comma-separated argument lists) and each
//! rule rewrites the components it knows; a value no rule applies to reads back as the tokens were.
//!
//! | rule | written | read back |
//! |---|---|---|
//! | a hex or functional colour is `rgb()`/`rgba()` | `#FFF`, `hsl(0,100%,50%)`, `rgb(0 0 0 / 50%)` | `rgb(255, 255, 255)`, `rgb(255, 0, 0)`, `rgba(0, 0, 0, 0.5)` |
//! | a hex alpha is 8-bit, shortest round trip | `#ff000080`, `#f008` | `rgba(255, 0, 0, 0.5)`, `rgba(255, 0, 0, 0.533)` |
//! | a colour keyword stays a keyword, lowercased | `RED`, `currentColor` | `red`, `currentcolor` |
//! | a bare `0` length has its unit | `margin: 0 auto`, `blur(0)`, `rotate(0)` | `0px auto`, `blur(0px)`, `rotate(0deg)` |
//! | four equal box sides collapse | `padding: 0 0 0 0`, `inset(0 0 0 0)` | `0px`, `inset(0px)` |
//! | a shadow's colour comes first, `inset` last | `inset 0 0 1px #000` | `rgb(0, 0, 0) 0px 0px 1px inset` |
//! | `calc()` is simplified, terms sorted | `calc(2 * (10px + 5px))`, `calc(1em + 1px + 1%)` | `calc(30px)`, `calc(1% + 1em + 1px)` |
//! | `url()` is quoted | `url(x.png)` | `url("x.png")` |
//! | `background` is image, repeat, colour | `#abc url(x.png) no-repeat` | `url("x.png") no-repeat rgb(170, 187, 204)` |
//! | `transition` drops its defaults | `opacity .3s ease 0s` | `opacity 0.3s` |

use crate::value::Color;

/// `value` (already in S31's token form) as Chrome reads back a declaration of `prop`.
pub fn canonical(prop: &str, value: &str) -> String {
    let prop = prop.to_ascii_lowercase();
    if prop.starts_with("--") {
        return value.to_string();
    }
    // an alpha value's percentage is its number (`ConsumeNumberOrPercent`; Chrome 150 reads `style.opacity = "25%"`
    // back as `0.25`) -- `serialize::value_text` has the rule
    if matches!(prop.as_str(), "opacity" | "fill-opacity" | "stroke-opacity" | "flood-opacity" | "stop-opacity" | "shape-image-threshold")
        && value.trim().ends_with('%')
    {
        let v = crate::style::serialize::value_text(&prop, value);
        if !v.ends_with('%') {
            return v;
        }
    }
    let Some(mut segs) = parse(value) else { return value.to_string() };
    let colorish = color_context(&prop);
    for seg in &mut segs {
        for n in seg.iter_mut() {
            rewrite(n, colorish);
        }
    }
    if LENGTH_PROPS.contains(&prop.as_str()) {
        for seg in &mut segs {
            for n in seg.iter_mut() {
                zero_unit(n, "px");
            }
        }
    }
    if prop == "flex" {
        // the third component is the basis, a length
        if let Some(seg) = segs.first_mut() {
            if seg.len() == 3 {
                zero_unit(&mut seg[2], "px");
            }
        }
    }
    if FOUR_SIDES.contains(&prop.as_str()) && segs.len() == 1 {
        collapse_sides(&mut segs[0]);
    }
    if prop == "box-shadow" || prop == "text-shadow" {
        for seg in &mut segs {
            shadow(seg, prop == "box-shadow");
        }
    }
    if prop == "background" && segs.len() == 1 {
        background(&mut segs[0]);
    }
    if prop == "transition" {
        for seg in &mut segs {
            transition(seg);
        }
    }
    serialize(&segs)
}

// ── components ──────────────────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
enum Node {
    Tok(String),
    Func(String, Vec<Vec<Node>>),
}

/// Comma-separated segments of space-separated components. `None` for text this parse cannot follow (an
/// unbalanced parenthesis), which then reads back as written.
fn parse(s: &str) -> Option<Vec<Vec<Node>>> {
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    let segs = parse_segs(&b, &mut i)?;
    (i == b.len()).then_some(segs)
}

fn parse_segs(b: &[char], i: &mut usize) -> Option<Vec<Vec<Node>>> {
    let mut segs = vec![Vec::new()];
    while *i < b.len() {
        let c = b[*i];
        if c == ' ' {
            *i += 1;
            continue;
        }
        if c == ')' {
            return Some(segs);
        }
        if c == ',' {
            segs.push(Vec::new());
            *i += 1;
            continue;
        }
        let start = *i;
        if c == '"' || c == '\'' {
            *i += 1;
            while *i < b.len() && b[*i] != c {
                if b[*i] == '\\' {
                    *i += 1;
                }
                *i += 1;
            }
            *i = (*i + 1).min(b.len());
            segs.last_mut()?.push(Node::Tok(b[start..*i].iter().collect()));
            continue;
        }
        while *i < b.len() && !matches!(b[*i], ' ' | ',' | '(' | ')') {
            *i += 1;
        }
        let atom: String = b[start..*i].iter().collect();
        if *i < b.len() && b[*i] == '(' {
            if atom.eq_ignore_ascii_case("url") {
                // `url(...)` whole, through its closing parenthesis
                while *i < b.len() && b[*i] != ')' {
                    *i += 1;
                }
                *i = (*i + 1).min(b.len());
                segs.last_mut()?.push(Node::Tok(b[start..*i].iter().collect()));
                continue;
            }
            *i += 1;
            let args = parse_segs(b, i)?;
            if *i >= b.len() || b[*i] != ')' {
                return None;
            }
            *i += 1;
            segs.last_mut()?.push(Node::Func(atom, args));
            continue;
        }
        if atom.is_empty() {
            return None;
        }
        segs.last_mut()?.push(Node::Tok(atom));
    }
    Some(segs)
}

fn serialize(segs: &[Vec<Node>]) -> String {
    segs.iter().map(|seg| seg.iter().map(node_text).collect::<Vec<_>>().join(" ")).collect::<Vec<_>>().join(", ")
}

fn node_text(n: &Node) -> String {
    match n {
        Node::Tok(t) => t.clone(),
        Node::Func(name, args) => format!("{name}({})", serialize(args)),
    }
}

// ── rules ───────────────────────────────────────────────────────────────────────────────────────────────────

/// The properties whose bare `0` components are lengths (Chrome writes them `0px`).
const LENGTH_PROPS: &[&str] = &[
    "margin", "margin-top", "margin-right", "margin-bottom", "margin-left", "margin-block", "margin-block-start",
    "margin-block-end", "margin-inline", "margin-inline-start", "margin-inline-end",
    "padding", "padding-top", "padding-right", "padding-bottom", "padding-left", "padding-block", "padding-block-start",
    "padding-block-end", "padding-inline", "padding-inline-start", "padding-inline-end",
    "top", "right", "bottom", "left", "inset", "inset-block", "inset-inline", "inset-block-start", "inset-block-end",
    "inset-inline-start", "inset-inline-end",
    "width", "height", "min-width", "min-height", "max-width", "max-height", "block-size", "inline-size",
    "min-block-size", "min-inline-size", "max-block-size", "max-inline-size",
    "border-width", "border-top-width", "border-right-width", "border-bottom-width", "border-left-width",
    "border", "border-top", "border-right", "border-bottom", "border-left", "outline", "outline-width", "outline-offset",
    "border-radius", "border-top-left-radius", "border-top-right-radius", "border-bottom-right-radius",
    "border-bottom-left-radius", "background-position", "background-position-x", "background-position-y",
    "gap", "row-gap", "column-gap", "grid-gap", "text-indent", "letter-spacing", "word-spacing", "flex-basis",
    "font-size", "perspective", "transform-origin", "perspective-origin", "object-position", "column-width",
    "border-spacing", "scroll-margin", "scroll-padding",
];

/// The box shorthands whose four sides collapse the way Chrome writes them.
const FOUR_SIDES: &[&str] = &[
    "margin", "padding", "inset", "border-width", "border-color", "border-style", "scroll-margin", "scroll-padding",
];

/// Whether a colour KEYWORD in this property's value is a colour (and so is lowercased): its name says colour,
/// or it is one of the shorthands and shadow properties a colour is a component of.
fn color_context(prop: &str) -> bool {
    prop.contains("color")
        || matches!(
            prop,
            "background" | "border" | "border-top" | "border-right" | "border-bottom" | "border-left" | "border-block"
                | "border-inline" | "outline" | "box-shadow" | "text-shadow" | "text-decoration" | "column-rule"
                | "fill" | "stroke" | "text-emphasis" | "-webkit-text-stroke" | "filter"
        )
}

/// Colours, `calc()`, the length and angle arguments of functions, `url()`, at any depth.
fn rewrite(n: &mut Node, colorish: bool) {
    if let Some(c) = color_text(n, colorish) {
        *n = Node::Tok(c);
        return;
    }
    match n {
        Node::Tok(t) => {
            // the prefix compared as bytes: a token is the page's text, and `t[..4]` of `"éé"` cuts the `é` and
            // panicked inside the style setter (an abort, exit 134, measured 2026-10-23)
            if t.len() > 4 && t.as_bytes()[..4].eq_ignore_ascii_case(b"url(") {
                *t = quote_url(t);
            }
        }
        Node::Func(name, args) => {
            let lower = name.to_ascii_lowercase();
            // Every math function -- `calc()`, `min()`, `round()`, `sin()`, ... -- as Blink serializes its parsed,
            // simplified expression (`style::calc`). The caller has already written each one from
            // the page's own text at full precision (`calc::serialize_math_in`), so this pass is idempotent on it.
            if crate::style::calc::is_math_function(&lower) {
                if let Some(e) = crate::style::calc::parse(&node_text(n)) {
                    *n = Node::Tok(e.css_text());
                }
                return;
            }
            // drop-shadow() is a shadow and its colour one whatever the property
            let inner_colorish = colorish || lower == "drop-shadow";
            for seg in args.iter_mut() {
                for a in seg.iter_mut() {
                    rewrite(a, inner_colorish);
                }
            }
            match lower.as_str() {
                "translate" | "translatex" | "translatey" | "translatez" | "translate3d" | "perspective" | "blur"
                | "polygon" | "circle" | "ellipse" => each_arg(args, |a| zero_unit(a, "px")),
                "inset" => {
                    each_arg(args, |a| zero_unit(a, "px"));
                    if args.len() == 1 {
                        collapse_sides(&mut args[0]);
                    }
                }
                "rotate" | "rotatex" | "rotatey" | "rotatez" | "skew" | "skewx" | "skewy" | "hue-rotate" => {
                    each_arg(args, |a| zero_unit(a, "deg"))
                }
                "drop-shadow" if args.len() == 1 => shadow(&mut args[0], false),
                _ => {}
            }
        }
    }
}

fn each_arg(args: &mut [Vec<Node>], mut f: impl FnMut(&mut Node)) {
    for seg in args.iter_mut() {
        for a in seg.iter_mut() {
            f(a);
        }
    }
}

/// A bare `0` component, with `unit`.
fn zero_unit(n: &mut Node, unit: &str) {
    if let Node::Tok(t) = n {
        if t == "0" || t == "-0" || t == "+0" {
            *t = format!("0{unit}");
        }
    }
}

/// `top right bottom left` as the fewest values that say the same (CSS box shorthands).
fn collapse_sides(seg: &mut Vec<Node>) {
    let t: Vec<String> = seg.iter().map(node_text).collect();
    let keep = match t.len() {
        4 if t[1] == t[3] && t[0] == t[2] && t[0] == t[1] => 1,
        4 if t[1] == t[3] && t[0] == t[2] => 2,
        4 if t[1] == t[3] => 3,
        3 if t[0] == t[2] && t[0] == t[1] => 1,
        3 if t[0] == t[2] => 2,
        2 if t[0] == t[1] => 1,
        n => n,
    };
    seg.truncate(keep);
}

/// One shadow: its colour first, then its lengths (a bare `0` with `px`), then `inset`.
fn shadow(seg: &mut Vec<Node>, allow_inset: bool) {
    let mut color = None;
    let mut inset = false;
    let mut lengths = Vec::new();
    for n in seg.drain(..) {
        match &n {
            Node::Tok(t) if allow_inset && t.eq_ignore_ascii_case("inset") => inset = true,
            _ if color.is_none() && is_color(&n) => color = Some(n),
            _ => {
                let mut n = n;
                zero_unit(&mut n, "px");
                lengths.push(n);
            }
        }
    }
    seg.extend(color);
    seg.extend(lengths);
    if inset {
        seg.push(Node::Tok("inset".into()));
    }
}

/// `background` (one layer): image, position/size, repeat, attachment, boxes, colour -- Chrome's order.
fn background(seg: &mut Vec<Node>) {
    let (mut image, mut pos, mut repeat, mut attach, mut boxes, mut color) = (vec![], vec![], vec![], vec![], vec![], vec![]);
    for n in seg.drain(..) {
        let t = node_text(&n).to_ascii_lowercase();
        if is_color(&n) {
            color.push(n);
        } else if t.starts_with("url(") || t.contains("gradient(") || t == "none" || t.starts_with("image-set(") {
            image.push(n);
        } else if matches!(t.as_str(), "repeat" | "repeat-x" | "repeat-y" | "no-repeat" | "space" | "round") {
            repeat.push(n);
        } else if matches!(t.as_str(), "scroll" | "fixed" | "local") {
            attach.push(n);
        } else if matches!(t.as_str(), "border-box" | "padding-box" | "content-box" | "text") {
            boxes.push(n);
        } else {
            pos.push(n);
        }
    }
    for part in [image, pos, repeat, attach, boxes, color] {
        seg.extend(part);
    }
}

/// `transition` (one item): the default timing function (`ease`) and the default delay (`0s`) are left out.
fn transition(seg: &mut Vec<Node>) {
    let mut times = 0;
    seg.retain(|n| {
        let t = node_text(n).to_ascii_lowercase();
        if t == "ease" {
            return false;
        }
        let time = t.ends_with("ms") || (t.ends_with('s') && t[..t.len() - 1].parse::<f64>().is_ok());
        if time {
            times += 1;
            if times == 2 && (t == "0s" || t == "0ms") {
                return false;
            }
        }
        true
    });
}

// ── colours ─────────────────────────────────────────────────────────────────────────────────────────────────

fn is_color(n: &Node) -> bool {
    match n {
        Node::Tok(t) => t.starts_with('#') || Color::parse_str(t).is_ok(),
        Node::Func(name, _) => matches!(
            name.to_ascii_lowercase().as_str(),
            "rgb" | "rgba" | "hsl" | "hsla" | "hwb" | "lab" | "lch" | "oklab" | "oklch" | "color" | "color-mix" | "contrast-color"
        ),
    }
}

/// The colour `n` is, as Chrome writes it: a hex or `rgb()`/`hsl()` function as `rgb()`/`rgba()`, a keyword (only
/// where the property takes colours) lowercased. `None` when `n` is not one this can say.
fn color_text(n: &Node, colorish: bool) -> Option<String> {
    match n {
        // Blink's `CSSColor::CustomCSSText`: the colour as `SerializeAsCSSColor` writes it
        // (`lab(50 40 30)` as written, `rgba(0, 0, 0, 0.503)` as `rgba(0, 0, 0, 0.5)`).
        Node::Tok(t) if t.starts_with('#') => {
            let c = Color::parse_str(t).ok()?;
            Some(c.serialize())
        }
        Node::Tok(t) if colorish && t.chars().all(|c| c.is_ascii_alphabetic()) && Color::parse_str(t).is_ok() => {
            Some(t.to_ascii_lowercase())
        }
        Node::Func(name, _)
            if matches!(
                name.to_ascii_lowercase().as_str(),
                "rgb" | "rgba" | "hsl" | "hsla" | "hwb" | "lab" | "lch" | "oklab" | "oklch" | "color" | "color-mix" | "contrast-color"
            ) =>
        {
            // A function resolved at parse time as its colour; `color-mix()`, a relative colour, `calc()` in a
            // lightness-first space as written (`CSSColorMixValue::CustomCSSText`, ...).
            Some(crate::css_color::parse_color_value(&node_text(n))?.css_text())
        }
        _ => None,
    }
}

/// `t` starts with an ASCII `url(`, so byte 4 is a character boundary. The token runs through its `)`, or to the
/// end of the value when the page left it open (CSS Syntax 3 §4.3.6 ends a url token at EOF): only a `)` is
/// dropped. Dropping the last BYTE cut a final `é` (`url(é` panicked) and ate the last character of an open
/// `url(x.png` (read back as `url("x.pn")`).
fn quote_url(t: &str) -> String {
    let body = &t[4..];
    let inner = body.strip_suffix(')').unwrap_or(body).trim();
    let inner = inner
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| inner.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(inner);
    format!("url(\"{}\")", inner.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::canonical;

    /// Every row as Chrome 150 read it back (`tools/realweb/seed/declcanon`, measured 2026-09-30), from the
    /// S31 token form the declaration arrives in.
    #[test]
    fn declared_values_read_back_as_chrome_writes_them() {
        for (prop, written, chrome) in [
            ("color", "#FFF", "rgb(255, 255, 255)"),
            ("color", "#ff000080", "rgba(255, 0, 0, 0.5)"),
            ("color", "#f008", "rgba(255, 0, 0, 0.533)"),
            ("color", "red", "red"),
            ("color", "RED", "red"),
            ("color", "transparent", "transparent"),
            ("color", "currentColor", "currentcolor"),
            ("color", "rgb(0 0 0 / 50%)", "rgba(0, 0, 0, 0.5)"),
            ("color", "rgba(0, 0, 0, 1)", "rgb(0, 0, 0)"),
            ("color", "rgb(255, 0, 0, 0.25)", "rgba(255, 0, 0, 0.25)"),
            ("color", "hsl(0, 100%, 50%)", "rgb(255, 0, 0)"),
            ("color", "hsla(120, 100%, 25%, 0.5)", "rgba(0, 128, 0, 0.5)"),
            ("color", "rgb(100%, 0%, 0%)", "rgb(255, 0, 0)"),
            ("color", "rgb(300, -5, 0)", "rgb(255, 0, 0)"),
            ("border", "1px solid #FFF", "1px solid rgb(255, 255, 255)"),
            ("border-color", "#000 red", "rgb(0, 0, 0) red"),
            ("background", "#abc url(x.png) no-repeat", "url(\"x.png\") no-repeat rgb(170, 187, 204)"),
            ("background-color", "#ABC", "rgb(170, 187, 204)"),
            ("box-shadow", "inset 0 0 1px #000", "rgb(0, 0, 0) 0px 0px 1px inset"),
            ("box-shadow", "0 0 1px 2px rgba(0, 0, 0, 0.5) inset", "rgba(0, 0, 0, 0.5) 0px 0px 1px 2px inset"),
            ("box-shadow", "none", "none"),
            ("box-shadow", "0 0 1px rgba(0, 0, 0, 0.5)", "rgba(0, 0, 0, 0.5) 0px 0px 1px"),
            ("box-shadow", "1px 1px red, 2px 2px blue", "red 1px 1px, blue 2px 2px"),
            ("text-shadow", "1px 1px 2px black", "black 1px 1px 2px"),
            ("text-shadow", "red 0 0", "red 0px 0px"),
            ("box-shadow", "1px 2px", "1px 2px"),
            ("filter", "drop-shadow(1px 1px red) blur(2px)", "drop-shadow(red 1px 1px) blur(2px)"),
            ("filter", "drop-shadow(0 0 1px rgba(0, 0, 0, 0.5))", "drop-shadow(rgba(0, 0, 0, 0.5) 0px 0px 1px)"),
            ("filter", "blur(0)", "blur(0px)"),
            ("width", "calc(20px + 100%)", "calc(100% + 20px)"),
            ("width", "calc(1px + 2em)", "calc(2em + 1px)"),
            ("width", "calc(10px * 2)", "calc(20px)"),
            ("width", "calc(100px / 4)", "calc(25px)"),
            ("width", "calc(50% - 10px + 5px)", "calc(50% - 5px)"),
            ("width", "calc(2 * (10px + 5px))", "calc(30px)"),
            ("width", "calc(1em + 1px + 1%)", "calc(1% + 1em + 1px)"),
            ("width", "calc(0px)", "calc(0px)"),
            ("width", "calc(10px - 10px)", "calc(0px)"),
            ("width", "calc(10px + 2px)", "calc(12px)"),
            ("width", "calc(100% - (2*10px))", "calc(100% - 20px)"),
            ("margin", "0", "0px"),
            ("margin", "0 auto", "0px auto"),
            ("padding", "0 0 0 0", "0px"),
            ("top", "0", "0px"),
            ("line-height", "0", "0"),
            ("flex", "1 1 0", "1 1 0px"),
            ("transform", "translate(0, 0)", "translate(0px, 0px)"),
            ("transform", "rotate(0)", "rotate(0deg)"),
            ("clip-path", "polygon(0 0, 100% 0, 100% 100%)", "polygon(0px 0px, 100% 0px, 100% 100%)"),
            ("clip-path", "inset(0 0 0 0)", "inset(0px)"),
            ("clip-path", "circle(50% at 0 0)", "circle(50% at 0px 0px)"),
            ("opacity", "0.5", "0.5"),
            ("z-index", "10", "10"),
            ("transition", "opacity 0.3s ease 0s", "opacity 0.3s"),
            ("background-position", "0 0", "0px 0px"),
            ("border-radius", "0", "0px"),
            ("outline", "0", "0px"),
        ] {
            assert_eq!(canonical(prop, written), chrome, "{prop}: {written}");
        }
    }

    /// A string or `url()` with a multi-byte character where the rules slice: `"éé"` puts byte 4 inside an `é`,
    /// `url(é` ends in one. Both aborted the process from the style setter (2026-10-23). The open url tokens read
    /// back closed, as CSS Syntax 3 ends a url token at EOF.
    #[test]
    fn non_ascii_strings_and_urls_read_back_whole() {
        for (prop, written, read) in [
            ("content", "\"éé\"", "\"éé\""),
            ("content", "\"😀\"", "\"😀\""),
            ("content", "\"ab\" \"éé\"", "\"ab\" \"éé\""),
            ("font-family", "\"éé\", serif", "\"éé\", serif"),
            ("background-image", "url(é)", "url(\"é\")"),
            ("background-image", "url(\"é\")", "url(\"é\")"),
            ("background-image", "url(é", "url(\"é\")"),
            ("background-image", "url(\"é\"", "url(\"é\")"),
            ("background-image", "url(x.png", "url(\"x.png\")"),
            ("background-image", "url()", "url(\"\")"),
            ("background", "url(é", "url(\"é\")"),
        ] {
            assert_eq!(canonical(prop, written), read, "{prop}: {written}");
        }
    }
}
