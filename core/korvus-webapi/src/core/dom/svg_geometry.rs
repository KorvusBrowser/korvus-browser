//! `core/dom/svg_geometry.rs` -- the SVG geometry and timeline members pages CALL:
//! `getBBox()`, `getTotalLength()`, `getPointAtLength()`, `ownerSVGElement`, and `SVGSVGElement`'s
//! `pauseAnimations()` / `unpauseAnimations()` / `animationsPaused()`.
//!
//! They were registry D2 stubs: `getBBox()` answered `undefined`, so `bbox.x` threw inside every chart
//! library that measures a label (d3 and everything built on it), and `getTotalLength()` answered 0,
//! so a stroke-dash "draw the line" animation drew nothing. The stub-call counter over the failing
//! replay rows found pages calling them (C3 FIND, `stubhits/`).
//!
//! Measured in Chrome 150 on `tools/realweb/seed/sx`: a `<rect x=10 y=20 width=30 height=40>` has the
//! bbox `10,20,30,40` (an `SVGRect`); `M0 0 L100 0 L100 100` has `0,0,100,100`, total length 200, and
//! the point at 150 is `(100, 50)` (an `SVGPoint`); `ownerSVGElement` is the nearest `<svg>` ancestor;
//! `animationsPaused()` is true after `pauseAnimations()`.
//!
//! WHAT IS APPROXIMATE, stated rather than hidden: curves (`C S Q T A`) are measured over a flattening
//! into short chords, so a curve's length and bbox are within a fraction of a pixel of the exact
//! answers but not bit-identical to Skia's -- Chrome's `getTotalLength()` for `<circle r=10>` is
//! 62.427, Skia's path measure over conic quarters, where this answers 2*pi*r = 62.832. Transforms are
//! not applied to the element's own box (a bbox is in its own user space, which is what `getBBox` is) -- a
//! container maps each child's by the child's transform, as Blink does -- and a text's box is its laid-out
//! characters' (`svg_text.rs`).

use deno_core::v8;

use crate::binding::protos::{throw_type_error, unwrap_id, wrap_cached};
use crate::binding::HostDom;
use crate::install::live::{getter_row, method_row};
use crate::install::MemberDef;

type Pt = (f64, f64);

/// How many chords a curve is flattened into.
const CURVE_STEPS: usize = 64;

fn num_attr(dom: &korvus_dom::Dom, id: u32, name: &str) -> f64 {
    dom.get_attribute(id, name).map(leading_number).unwrap_or(0.0)
}

/// The number an SVG length attribute starts with (`"10"`, `"10px"`, `" 1e1 "`); 0 when none.
fn leading_number(s: &str) -> f64 {
    let t = s.trim();
    let mut end = 0;
    for (i, c) in t.char_indices() {
        let ok = c.is_ascii_digit() || c == '.' || ((c == '-' || c == '+') && (i == 0 || t[..i].ends_with(['e', 'E']))) || ((c == 'e' || c == 'E') && i > 0);
        if !ok {
            break;
        }
        end = i + c.len_utf8();
    }
    t[..end].parse::<f64>().unwrap_or(0.0)
}

/// The numbers of a `points` attribute or of one path command's arguments.
fn numbers(s: &str) -> Vec<f64> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut Vec<f64>| {
        if !cur.is_empty() {
            if let Ok(n) = cur.parse::<f64>() {
                out.push(n);
            }
            cur.clear();
        }
    };
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c.is_ascii_digit() {
            cur.push(c);
        } else if c == '.' {
            if cur.contains('.') && !cur.contains(['e', 'E']) {
                flush(&mut cur, &mut out);
            }
            cur.push(c);
        } else if c == '-' || c == '+' {
            let after_exp = i > 0 && matches!(chars[i - 1], 'e' | 'E');
            if !after_exp {
                flush(&mut cur, &mut out);
            }
            cur.push(c);
        } else if (c == 'e' || c == 'E') && !cur.is_empty() {
            cur.push(c);
        } else {
            flush(&mut cur, &mut out);
        }
    }
    flush(&mut cur, &mut out);
    out
}

/// A path's outline as polylines (one per subpath), curves flattened.
fn path_polylines(d: &str) -> Vec<Vec<Pt>> {
    let mut subpaths: Vec<Vec<Pt>> = Vec::new();
    let mut cur: Vec<Pt> = Vec::new();
    let (mut x, mut y) = (0.0f64, 0.0f64);
    let (mut sx, mut sy) = (0.0f64, 0.0f64);
    let mut last_ctrl: Option<(char, Pt)> = None;
    let mut cmds: Vec<(char, Vec<f64>)> = Vec::new();
    let mut i = 0;
    let bytes: Vec<char> = d.chars().collect();
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_alphabetic() && c != 'e' && c != 'E' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && !(bytes[j].is_ascii_alphabetic() && bytes[j] != 'e' && bytes[j] != 'E') {
                j += 1;
            }
            let args: String = bytes[start..j].iter().collect();
            cmds.push((c, numbers(&args)));
            i = j;
        } else {
            i += 1;
        }
    }
    let push_line = |cur: &mut Vec<Pt>, p: Pt| cur.push(p);
    // ERROR HANDLING (SVG 2 §9.5.4): path data renders up to the last complete segment before the first
    // error -- an unknown command, an incomplete argument list, an arc flag other than 0 or 1 -- and
    // data that does not begin with a moveto renders nothing. WPT `svg/path/parsing/error-handling.html`.
    if !cmds.first().is_some_and(|(c, _)| matches!(c, 'M' | 'm')) {
        return Vec::new();
    }
    'commands: for (cmd, args) in cmds {
        let rel = cmd.is_ascii_lowercase();
        let up = cmd.to_ascii_uppercase();
        let arity = match up {
            'M' | 'L' | 'T' => 2,
            'H' | 'V' => 1,
            'C' => 6,
            'S' | 'Q' => 4,
            'A' => 7,
            'Z' => 0,
            _ => break 'commands,
        };
        if arity > 0 && args.len() < arity {
            break 'commands;
        }
        if up == 'Z' {
            if !cur.is_empty() {
                push_line(&mut cur, (sx, sy));
                subpaths.push(std::mem::take(&mut cur));
            }
            x = sx;
            y = sy;
            last_ctrl = None;
            continue;
        }
        let mut k = 0;
        let mut first = true;
        while k + arity <= args.len() {
            let a = &args[k..k + arity];
            k += arity;
            let (ox, oy) = if rel { (x, y) } else { (0.0, 0.0) };
            match up {
                'M' => {
                    if first {
                        if cur.len() > 1 {
                            subpaths.push(std::mem::take(&mut cur));
                        } else {
                            cur.clear();
                        }
                        x = ox + a[0];
                        y = oy + a[1];
                        sx = x;
                        sy = y;
                        cur.push((x, y));
                    } else {
                        // Implicit lineto after a moveto's first pair.
                        x = ox + a[0];
                        y = oy + a[1];
                        push_line(&mut cur, (x, y));
                    }
                    last_ctrl = None;
                }
                'L' => {
                    x = ox + a[0];
                    y = oy + a[1];
                    push_line(&mut cur, (x, y));
                    last_ctrl = None;
                }
                'H' => {
                    x = if rel { x + a[0] } else { a[0] };
                    push_line(&mut cur, (x, y));
                    last_ctrl = None;
                }
                'V' => {
                    y = if rel { y + a[0] } else { a[0] };
                    push_line(&mut cur, (x, y));
                    last_ctrl = None;
                }
                'C' | 'S' => {
                    let (c1, c2, end) = if up == 'C' {
                        ((ox + a[0], oy + a[1]), (ox + a[2], oy + a[3]), (ox + a[4], oy + a[5]))
                    } else {
                        let c1 = match last_ctrl {
                            Some(('C', (px, py))) => (2.0 * x - px, 2.0 * y - py),
                            _ => (x, y),
                        };
                        (c1, (ox + a[0], oy + a[1]), (ox + a[2], oy + a[3]))
                    };
                    let p0 = (x, y);
                    for s in 1..=CURVE_STEPS {
                        let t = s as f64 / CURVE_STEPS as f64;
                        let mt = 1.0 - t;
                        let px = mt * mt * mt * p0.0 + 3.0 * mt * mt * t * c1.0 + 3.0 * mt * t * t * c2.0 + t * t * t * end.0;
                        let py = mt * mt * mt * p0.1 + 3.0 * mt * mt * t * c1.1 + 3.0 * mt * t * t * c2.1 + t * t * t * end.1;
                        push_line(&mut cur, (px, py));
                    }
                    x = end.0;
                    y = end.1;
                    last_ctrl = Some(('C', c2));
                }
                'Q' | 'T' => {
                    let (c, end) = if up == 'Q' {
                        ((ox + a[0], oy + a[1]), (ox + a[2], oy + a[3]))
                    } else {
                        let c = match last_ctrl {
                            Some(('Q', (px, py))) => (2.0 * x - px, 2.0 * y - py),
                            _ => (x, y),
                        };
                        (c, (ox + a[0], oy + a[1]))
                    };
                    let p0 = (x, y);
                    for s in 1..=CURVE_STEPS {
                        let t = s as f64 / CURVE_STEPS as f64;
                        let mt = 1.0 - t;
                        let px = mt * mt * p0.0 + 2.0 * mt * t * c.0 + t * t * end.0;
                        let py = mt * mt * p0.1 + 2.0 * mt * t * c.1 + t * t * end.1;
                        push_line(&mut cur, (px, py));
                    }
                    x = end.0;
                    y = end.1;
                    last_ctrl = Some(('Q', c));
                }
                'A' => {
                    if !(a[3] == 0.0 || a[3] == 1.0) || !(a[4] == 0.0 || a[4] == 1.0) {
                        break 'commands;
                    }
                    let end = (ox + a[5], oy + a[6]);
                    for p in arc_points((x, y), a[0], a[1], a[2], a[3] != 0.0, a[4] != 0.0, end) {
                        push_line(&mut cur, p);
                    }
                    x = end.0;
                    y = end.1;
                    last_ctrl = None;
                }
                _ => {}
            }
            first = false;
        }
        // Arguments left over that do not make a whole segment: the error is here.
        if k < args.len() {
            break 'commands;
        }
    }
    if cur.len() > 1 {
        subpaths.push(cur);
    }
    subpaths
}

/// An elliptical arc (SVG endpoint parameterization, Appendix B.2.4) as points after `p0`.
fn arc_points(p0: Pt, rx: f64, ry: f64, phi_deg: f64, large: bool, sweep: bool, p1: Pt) -> Vec<Pt> {
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if rx == 0.0 || ry == 0.0 || p0 == p1 {
        return vec![p1];
    }
    let phi = phi_deg.to_radians();
    let (cos, sin) = (phi.cos(), phi.sin());
    let dx = (p0.0 - p1.0) / 2.0;
    let dy = (p0.1 - p1.1) / 2.0;
    let x1 = cos * dx + sin * dy;
    let y1 = -sin * dx + cos * dy;
    let lambda = (x1 * x1) / (rx * rx) + (y1 * y1) / (ry * ry);
    if lambda > 1.0 {
        rx *= lambda.sqrt();
        ry *= lambda.sqrt();
    }
    let num = rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1;
    let den = rx * rx * y1 * y1 + ry * ry * x1 * x1;
    let mut coef = if den == 0.0 { 0.0 } else { (num / den).max(0.0).sqrt() };
    if large == sweep {
        coef = -coef;
    }
    let cx1 = coef * rx * y1 / ry;
    let cy1 = -coef * ry * x1 / rx;
    let cx = cos * cx1 - sin * cy1 + (p0.0 + p1.0) / 2.0;
    let cy = sin * cx1 + cos * cy1 + (p0.1 + p1.1) / 2.0;
    let angle = |ux: f64, uy: f64, vx: f64, vy: f64| -> f64 {
        let dot = ux * vx + uy * vy;
        let len = (ux * ux + uy * uy).sqrt() * (vx * vx + vy * vy).sqrt();
        let mut a = (dot / len).clamp(-1.0, 1.0).acos();
        if ux * vy - uy * vx < 0.0 {
            a = -a;
        }
        a
    };
    let theta1 = angle(1.0, 0.0, (x1 - cx1) / rx, (y1 - cy1) / ry);
    let mut dtheta = angle((x1 - cx1) / rx, (y1 - cy1) / ry, (-x1 - cx1) / rx, (-y1 - cy1) / ry);
    if !sweep && dtheta > 0.0 {
        dtheta -= std::f64::consts::TAU;
    } else if sweep && dtheta < 0.0 {
        dtheta += std::f64::consts::TAU;
    }
    (1..=CURVE_STEPS)
        .map(|s| {
            let t = theta1 + dtheta * (s as f64 / CURVE_STEPS as f64);
            let (ct, st) = (t.cos(), t.sin());
            (cos * rx * ct - sin * ry * st + cx, sin * rx * ct + cos * ry * st + cy)
        })
        .collect()
}

/// The element's outline as polylines, in its own user space. `None` for an element with no geometry.
fn outline(dom: &korvus_dom::Dom, id: u32) -> Option<Vec<Vec<Pt>>> {
    let tag = dom.tag_name(id)?.rsplit(':').next()?.to_string();
    let n = |a: &str| num_attr(dom, id, a);
    Some(match tag.as_str() {
        "path" => path_polylines(dom.get_attribute(id, "d").unwrap_or("")),
        "rect" => {
            let (x, y, w, h) = (n("x"), n("y"), n("width"), n("height"));
            vec![vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h), (x, y)]]
        }
        "line" => vec![vec![(n("x1"), n("y1")), (n("x2"), n("y2"))]],
        "polyline" | "polygon" => {
            let v = numbers(dom.get_attribute(id, "points").unwrap_or(""));
            let mut pts: Vec<Pt> = v.chunks_exact(2).map(|c| (c[0], c[1])).collect();
            if tag == "polygon" {
                if let Some(&p) = pts.first() {
                    pts.push(p);
                }
            }
            vec![pts]
        }
        "circle" | "ellipse" => {
            let (cx, cy) = (n("cx"), n("cy"));
            let (rx, ry) = if tag == "circle" { (n("r"), n("r")) } else { (n("rx"), n("ry")) };
            let steps = 4 * CURVE_STEPS;
            vec![(0..=steps)
                .map(|s| {
                    let t = std::f64::consts::TAU * s as f64 / steps as f64;
                    (cx + rx * t.cos(), cy + ry * t.sin())
                })
                .collect()]
        }
        _ => return None,
    })
}

/// Whether `el` is the OUTERMOST `<svg>` of a document: connected, and not inside another `<svg>` (the
/// one `currentScale` zooms).
pub(crate) fn is_outermost_svg(scope: &mut v8::PinScope, el: v8::Local<v8::Object>) -> bool {
    if !crate::page::is_connected(scope, el.into()) {
        return false;
    }
    let Some(id) = unwrap_id(scope, el.into()) else { return false };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    is_svg_root(&st.borrow::<HostDom>().0, id)
}

/// Whether the shape's path is EMPTY in Skia's sense (no segment at all): a `<path>` with no command, a
/// poly shape with no point, a rect or ellipse with a non-positive extent.
fn path_is_empty(dom: &korvus_dom::Dom, id: u32) -> bool {
    let tag = dom.tag_name(id).and_then(|t| t.rsplit(':').next()).unwrap_or("").to_string();
    let n = |a: &str| num_attr(dom, id, a);
    match tag.as_str() {
        "rect" => n("width") <= 0.0 || n("height") <= 0.0,
        "circle" => n("r") <= 0.0,
        "ellipse" => n("rx") <= 0.0 || n("ry") <= 0.0,
        "line" => false,
        _ => outline(dom, id).is_none_or(|o| o.iter().all(|p| p.is_empty())),
    }
}

/// An element's object bounding box in its own user space (`ObjectBoundingBox`), `None` where Blink's has none:
/// a shape's ([`shape_bbox`]); a text's from `texts` (its laid-out characters, [`crate::core::dom::svg_text::text_bbox`]);
/// a container's the union, even of empty boxes, of what its children contribute (`HasValidBoundingBoxForContainer`:
/// a shape that is not empty, a text with characters, a container holding one -- never a hidden container), each
/// mapped by the child's own transform (`SVGContentContainer::UpdateBoundingBoxes`). Chrome 150: the challenge's
/// `<g>` of 19 texts under `scale(1.000998)` reads `x` 32.03193664550781, each text's box scaled into the group's
/// space; this read the texts' boxes unscaled (and before that, a text had no box at all).
fn object_bbox(dom: &korvus_dom::Dom, id: u32, texts: &std::collections::HashMap<u32, Option<[f32; 4]>>, transforms: &std::collections::HashMap<u32, Matrix>) -> Option<[f32; 4]> {
    if let Some(&b) = texts.get(&id) {
        return b;
    }
    let tag = dom.tag_name(id).and_then(|t| t.rsplit(':').next()).unwrap_or("").to_string();
    match tag.as_str() {
        "rect" | "circle" | "ellipse" | "line" | "path" | "polyline" | "polygon" => {
            let (x, y, w, h) = shape_bbox(dom, id);
            Some([x as f32, y as f32, w as f32, h as f32])
        }
        "g" | "a" | "switch" | "svg" => {
            let mut out: Option<[f32; 4]> = None;
            for c in dom.child_elements(id) {
                let shape = matches!(
                    dom.tag_name(c).and_then(|t| t.rsplit(':').next()),
                    Some("rect" | "circle" | "ellipse" | "line" | "path" | "polyline" | "polygon")
                );
                if shape && path_is_empty(dom, c) {
                    continue;
                }
                let Some(b) = object_bbox(dom, c, texts, transforms) else { continue };
                let b = transforms.get(&c).map_or(b, |m| map_rect(*m, b));
                out = Some(out.map_or(b, |o| union_even_if_empty(o, b)));
            }
            out
        }
        _ => None,
    }
}

/// A shape's exact bbox where one exists in closed form; the outline's extent otherwise.
fn shape_bbox(dom: &korvus_dom::Dom, id: u32) -> (f64, f64, f64, f64) {
    let tag = dom.tag_name(id).and_then(|t| t.rsplit(':').next()).unwrap_or("").to_string();
    let n = |a: &str| num_attr(dom, id, a);
    match tag.as_str() {
        "rect" => return (n("x"), n("y"), n("width"), n("height")),
        "circle" => {
            let r = n("r");
            return (n("cx") - r, n("cy") - r, 2.0 * r, 2.0 * r);
        }
        "ellipse" => {
            let (rx, ry) = (n("rx"), n("ry"));
            return (n("cx") - rx, n("cy") - ry, 2.0 * rx, 2.0 * ry);
        }
        _ => {}
    }
    let pts: Vec<Pt> = outline(dom, id).unwrap_or_default().into_iter().flatten().collect();
    if pts.is_empty() {
        return (0.0, 0.0, 0.0, 0.0);
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for (x, y) in pts {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    (x0, y0, x1 - x0, y1 - y0)
}

fn seg_len(a: Pt, b: Pt) -> f64 {
    ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt()
}

fn total_length(dom: &korvus_dom::Dom, id: u32) -> f64 {
    let tag = dom.tag_name(id).and_then(|t| t.rsplit(':').next()).unwrap_or("").to_string();
    if tag == "circle" {
        return std::f64::consts::TAU * num_attr(dom, id, "r");
    }
    // `+ 0.0`: an empty `f64` sum is -0, and `getTotalLength()` of an empty path is +0.
    outline(dom, id).unwrap_or_default().iter().map(|p| p.windows(2).map(|w| seg_len(w[0], w[1])).sum::<f64>()).sum::<f64>() + 0.0
}

fn point_at(dom: &korvus_dom::Dom, id: u32, dist: f64) -> Pt {
    let lines = outline(dom, id).unwrap_or_default();
    let first = lines.first().and_then(|l| l.first()).copied().unwrap_or((0.0, 0.0));
    let mut left = dist.max(0.0);
    let mut last = first;
    for l in &lines {
        for w in l.windows(2) {
            let d = seg_len(w[0], w[1]);
            if left <= d && d > 0.0 {
                let t = left / d;
                return (w[0].0 + (w[1].0 - w[0].0) * t, w[0].1 + (w[1].1 - w[0].1) * t);
            }
            left -= d;
            last = w[1];
        }
    }
    last
}

fn element_of(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> Option<u32> {
    let id = unwrap_id(scope, args.this().into());
    if id.is_none() {
        throw_type_error(scope, "Illegal invocation");
    }
    id
}

fn cb_get_bbox(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = element_of(scope, &args) else { return };
    // What the box reads from layout and style, for the element and every element under it: a text's laid-out
    // characters (`svg_text`) and each element's own transform.
    let below: Vec<u32> = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        let mut out = Vec::new();
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            out.push(n);
            if !crate::core::dom::svg_text::is_text_element(dom, n) {
                stack.extend(dom.child_elements(n));
            }
        }
        out
    };
    let mut texts: std::collections::HashMap<u32, Option<[f32; 4]>> = std::collections::HashMap::new();
    let mut transforms: std::collections::HashMap<u32, Matrix> = std::collections::HashMap::new();
    for &n in &below {
        let is_text = {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            crate::core::dom::svg_text::is_text_element(&st.borrow::<HostDom>().0, n)
        };
        if is_text {
            texts.insert(n, crate::core::dom::svg_text::text_bbox(scope, n));
        }
        if n != id {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let mut st = op_state.borrow_mut();
            if let Some(m) = local_transform(&mut st, n) {
                transforms.insert(n, m);
            }
        }
    }
    let b = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        object_bbox(&st.borrow::<HostDom>().0, id, &texts, &transforms).unwrap_or([0.0; 4])
    };
    let fields = [("x", b[0] as f64), ("y", b[1] as f64), ("width", b[2] as f64), ("height", b[3] as f64)];
    if let Some(r) = crate::core::dom::svg_values::detached_value(scope, "SVGRect", &fields) {
        rv.set(r.into());
    }
}

/// An SVG element's own transform to its parent's user space (`LocalToSVGParentTransform`): its computed CSS
/// `transform`, else its `transform` attribute; `None` when it has neither.
fn local_transform(st: &mut deno_core::OpState, n: u32) -> Option<Matrix> {
    if let Some(m) = crate::core::css::cssom::transform_matrix_2d(st, n) {
        return Some(m);
    }
    let dom = &st.borrow::<HostDom>().0;
    dom.get_attribute(n, "transform").map(parse_transform).filter(|m| *m != IDENTITY)
}

/// Blink's `AffineTransform::MapRect` for a float box through `m` (double): a translation moves its origin; any
/// other transform maps its four corners in double, each made a float, and takes their bounding box in float.
fn map_rect(m: Matrix, r: [f32; 4]) -> [f32; 4] {
    let point = |x: f32, y: f32| ((m[0] * x as f64 + m[2] * y as f64 + m[4]) as f32, (m[1] * x as f64 + m[3] * y as f64 + m[5]) as f32);
    if m[0] == 1.0 && m[1] == 0.0 && m[2] == 0.0 && m[3] == 1.0 {
        let (x, y) = point(r[0], r[1]);
        return [x, y, r[2], r[3]];
    }
    let (right, bottom) = (r[0] + r[2], r[1] + r[3]);
    let ps = [point(r[0], r[1]), point(right, r[1]), point(right, bottom), point(r[0], bottom)];
    let (mut x0, mut y0, mut x1, mut y1) = (ps[0].0, ps[0].1, ps[0].0, ps[0].1);
    for &(x, y) in &ps[1..] {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    [x0, y0, x1 - x0, y1 - y0]
}

/// `gfx::RectF::UnionEvenIfEmpty`, in float.
fn union_even_if_empty(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    let (l, t) = (a[0].min(b[0]), a[1].min(b[1]));
    let (r, bt) = ((a[0] + a[2]).max(b[0] + b[2]), (a[1] + a[3]).max(b[1] + b[3]));
    [l, t, r - l, bt - t]
}

fn cb_get_total_length(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = element_of(scope, &args) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let len = total_length(&op_state.borrow().borrow::<HostDom>().0, id);
    rv.set_double(len);
}

fn cb_get_point_at_length(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = element_of(scope, &args) else { return };
    let Some(n) = args.get(0).number_value(scope) else { return };
    if !n.is_finite() {
        throw_type_error(scope, "Failed to execute 'getPointAtLength' on 'SVGGeometryElement': The provided float value is non-finite.");
        return;
    }
    let ((x, y), empty) = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        (point_at(dom, id, n), path_is_empty(dom, id))
    };
    // Blink's SVGGeometryElement::getPointAtLength: a shape with no path is InvalidStateError
    // (measured on Chrome 150: a fresh <path>).
    if empty {
        crate::binding::protos::throw_dom(scope, "Failed to execute 'getPointAtLength' on 'SVGGeometryElement': The element's path is empty.", "InvalidStateError");
        return;
    }
    if let Some(p) = crate::core::dom::svg_values::detached_value(scope, "SVGPoint", &[("x", x), ("y", y)]) {
        rv.set(p.into());
    }
}

/// `ownerSVGElement`: the nearest ancestor `<svg>`, or null for the outermost one.
fn cb_owner_svg(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = element_of(scope, &args) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let owner = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        let mut cur = dom.parent(id);
        let mut found = None;
        while let Some(p) = cur {
            if dom.node_type(p) == 1 && dom.tag_name(p).and_then(|t| t.rsplit(':').next()) == Some("svg") && !dom.is_html_ns(p) {
                found = Some(p);
                break;
            }
            cur = dom.parent(p);
        }
        found
    };
    match owner {
        Some(o) => rv.set(wrap_cached(scope, &op_state, o).into()),
        None => rv.set_null(),
    }
}

/// The `<svg>` elements whose animation timeline is paused.
#[derive(Default)]
struct PausedTimelines(std::collections::HashSet<u32>);

fn set_paused(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, paused: bool) {
    let Some(id) = element_of(scope, args) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<PausedTimelines>().is_none() {
        st.put(PausedTimelines::default());
    }
    let set = &mut st.borrow_mut::<PausedTimelines>().0;
    if paused {
        set.insert(id);
    } else {
        set.remove(&id);
    }
}

fn cb_pause(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    set_paused(scope, &args, true);
}

fn cb_unpause(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    set_paused(scope, &args, false);
}

fn cb_paused(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = element_of(scope, &args) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let p = op_state.borrow().try_borrow::<PausedTimelines>().is_some_and(|s| s.0.contains(&id));
    rv.set_bool(p);
}

/// `kits::svg_geometry`'s body.
// ── transforms: `getCTM`, `getScreenCTM`, `SVGMatrix`, `SVGPoint.matrixTransform` ───────────────
//
// d3's pointer code -- `svg.createSVGPoint()`, then `point.matrixTransform(node.getScreenCTM()
// .inverse())` -- is how every interactive chart turns a mouse position into data coordinates, and
// all four members were D2 stubs. A matrix is `[a, b, c, d, e, f]`, the SVG order.

type Matrix = [f64; 6];

const IDENTITY: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// `m * n`: `n` applied first.
fn mul(m: Matrix, n: Matrix) -> Matrix {
    [
        m[0] * n[0] + m[2] * n[1],
        m[1] * n[0] + m[3] * n[1],
        m[0] * n[2] + m[2] * n[3],
        m[1] * n[2] + m[3] * n[3],
        m[0] * n[4] + m[2] * n[5] + m[4],
        m[1] * n[4] + m[3] * n[5] + m[5],
    ]
}

fn inverse(m: Matrix) -> Option<Matrix> {
    let det = m[0] * m[3] - m[1] * m[2];
    if det == 0.0 || !det.is_finite() {
        return None;
    }
    Some([
        m[3] / det,
        -m[1] / det,
        -m[2] / det,
        m[0] / det,
        (m[2] * m[5] - m[3] * m[4]) / det,
        (m[1] * m[4] - m[0] * m[5]) / det,
    ])
}

/// A `transform` attribute's matrix: its functions composed left to right.
fn parse_transform(text: &str) -> Matrix {
    let mut m = IDENTITY;
    let mut rest = text;
    while let Some(open) = rest.find('(') {
        let name = rest[..open].trim_matches(|c: char| c.is_whitespace() || c == ',').to_ascii_lowercase();
        let Some(close) = rest[open..].find(')') else { break };
        let args = numbers(&rest[open + 1..open + close]);
        rest = &rest[open + close + 1..];
        let a = |i: usize| args.get(i).copied();
        let f: Matrix = match name.as_str() {
            "matrix" if args.len() == 6 => [args[0], args[1], args[2], args[3], args[4], args[5]],
            "translate" => [1.0, 0.0, 0.0, 1.0, a(0).unwrap_or(0.0), a(1).unwrap_or(0.0)],
            "scale" => {
                let sx = a(0).unwrap_or(1.0);
                [sx, 0.0, 0.0, a(1).unwrap_or(sx), 0.0, 0.0]
            }
            "rotate" => {
                let r = a(0).unwrap_or(0.0).to_radians();
                let rot = [r.cos(), r.sin(), -r.sin(), r.cos(), 0.0, 0.0];
                match (a(1), a(2)) {
                    (Some(cx), Some(cy)) => mul(mul([1.0, 0.0, 0.0, 1.0, cx, cy], rot), [1.0, 0.0, 0.0, 1.0, -cx, -cy]),
                    _ => rot,
                }
            }
            "skewx" => [1.0, 0.0, a(0).unwrap_or(0.0).to_radians().tan(), 1.0, 0.0, 0.0],
            "skewy" => [1.0, a(0).unwrap_or(0.0).to_radians().tan(), 0.0, 1.0, 0.0, 0.0],
            _ => IDENTITY,
        };
        m = mul(m, f);
    }
    m
}

/// The linear part `[a, b, c, d]` of a `transform` attribute's matrix (what scales a text's font).
pub(crate) fn linear_part_of(text: &str) -> [f64; 4] {
    let m = parse_transform(text);
    [m[0], m[1], m[2], m[3]]
}

/// The linear part of an `<svg>`'s `viewBox` mapping into its viewport.
pub(crate) fn viewbox_linear(st: &mut deno_core::OpState, svg: u32) -> [f64; 4] {
    let (w, h) = viewport_size(st, svg);
    let m = viewbox(&st.borrow::<HostDom>().0, svg, w, h);
    [m[0], m[1], m[2], m[3]]
}

fn is_svg_root(dom: &korvus_dom::Dom, id: u32) -> bool {
    dom.tag_name(id).and_then(|t| t.rsplit(':').next()).is_some_and(|t| t.eq_ignore_ascii_case("svg"))
}

/// An `<svg>`'s `viewBox` mapping into a `width` x `height` viewport (`preserveAspectRatio` align and
/// `meet`/`slice`, or `none`).
fn viewbox(dom: &korvus_dom::Dom, svg: u32, width: f64, height: f64) -> Matrix {
    let Some(vb) = dom.get_attribute(svg, "viewBox").map(numbers).filter(|v| v.len() == 4 && v[2] > 0.0 && v[3] > 0.0) else {
        return IDENTITY;
    };
    let (sx, sy) = (width / vb[2], height / vb[3]);
    let par = dom.get_attribute(svg, "preserveAspectRatio").unwrap_or("xMidYMid meet").trim().to_string();
    if par.starts_with("none") {
        return [sx, 0.0, 0.0, sy, -vb[0] * sx, -vb[1] * sy];
    }
    let s = if par.contains("slice") { sx.max(sy) } else { sx.min(sy) };
    let frac = |min: &str, mid: &str| if par.contains(min) { 0.0 } else if par.contains(mid) || !par.contains('M') { 0.5 } else { 1.0 };
    let (fx, fy) = (frac("xMin", "xMid"), frac("YMin", "YMid"));
    [s, 0.0, 0.0, s, -vb[0] * s + (width - vb[2] * s) * fx, -vb[1] * s + (height - vb[3] * s) * fy]
}

/// The viewport size an `<svg>` maps its `viewBox` into: its `width`/`height` attributes, else its
/// laid-out content box.
fn viewport_size(st: &mut deno_core::OpState, svg: u32) -> (f64, f64) {
    let dom = &st.borrow::<HostDom>().0;
    let (w, h) = (dom.get_attribute(svg, "width").map(leading_number), dom.get_attribute(svg, "height").map(leading_number));
    if let (Some(w), Some(h)) = (w.filter(|v| *v > 0.0), h.filter(|v| *v > 0.0)) {
        return (w, h);
    }
    crate::core::dom::geometry_queries::element_content_size(st, svg)
}

/// The element's transform to its nearest viewport's coordinates (`getCTM`), or, `screen`, to the
/// viewport the page is drawn in (`getScreenCTM`). The nearest `<svg>`'s `viewBox` is in both, as in
/// Chrome; `screen` adds every enclosing `<svg>` and the outermost one's position on the page.
fn ctm(st: &mut deno_core::OpState, id: u32, screen: bool) -> Matrix {
    let mut m = IDENTITY;
    let mut cur = Some(id);
    let mut first_viewport = true;
    while let Some(n) = cur {
        let (is_svg, transform, parent) = {
            let dom = &st.borrow::<HostDom>().0;
            let parent = dom.parent(n).filter(|&p| matches!(dom.node(p).kind, korvus_dom::NodeKind::Element { .. }));
            (is_svg_root(dom, n) && n != id, dom.get_attribute(n, "transform").map(str::to_string), parent)
        };
        if is_svg {
            let (w, h) = viewport_size(st, n);
            let dom = &st.borrow::<HostDom>().0;
            m = mul(viewbox(dom, n, w, h), m);
            let outer = parent.is_none_or(|p| !dom_is_in_svg(dom, p));
            if !screen && first_viewport {
                return m;
            }
            first_viewport = false;
            if outer {
                let r = crate::core::dom::geometry_queries::element_viewport_rect(st, n);
                return mul([1.0, 0.0, 0.0, 1.0, r.0, r.1], m);
            }
            let (x, y) = (num_attr(dom, n, "x"), num_attr(dom, n, "y"));
            m = mul([1.0, 0.0, 0.0, 1.0, x, y], m);
        } else if let Some(t) = transform {
            m = mul(parse_transform(&t), m);
        }
        cur = parent;
    }
    m
}

/// Is `id` an SVG element, i.e. inside an `<svg>` (or one itself)?
fn dom_is_in_svg(dom: &korvus_dom::Dom, id: u32) -> bool {
    let mut cur = Some(id);
    while let Some(n) = cur {
        if is_svg_root(dom, n) {
            return true;
        }
        cur = dom.parent(n);
    }
    false
}

fn matrix_value<'s>(scope: &mut v8::PinScope<'s, '_>, m: Matrix) -> Option<v8::Local<'s, v8::Object>> {
    crate::core::dom::svg_values::detached_value(
        scope,
        "SVGMatrix",
        &[("a", m[0]), ("b", m[1]), ("c", m[2]), ("d", m[3]), ("e", m[4]), ("f", m[5])],
    )
}

const MATRIX_MEMBERS: [&str; 6] = ["a", "b", "c", "d", "e", "f"];

/// The receiver's matrix, or `Illegal invocation`.
fn this_matrix(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Option<Matrix> {
    let Some(v) = crate::core::dom::svg_values::detached_nums(scope, this, &MATRIX_MEMBERS, 0.0) else {
        throw_type_error(scope, "Illegal invocation");
        return None;
    };
    Some([v[0], v[1], v[2], v[3], v[4], v[5]])
}

fn cb_get_ctm(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = element_of(scope, &args) else { return };
    let m = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        ctm(&mut st, id, false)
    };
    if let Some(o) = matrix_value(scope, m) {
        rv.set(o.into());
    }
}

fn cb_get_screen_ctm(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(id) = element_of(scope, &args) else { return };
    let m = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        ctm(&mut st, id, true)
    };
    if let Some(o) = matrix_value(scope, m) {
        rv.set(o.into());
    }
}

fn cb_create_svg_matrix(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if element_of(scope, &args).is_none() {
        return;
    }
    if let Some(o) = matrix_value(scope, IDENTITY) {
        rv.set(o.into());
    }
}

fn cb_matrix_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let Some(m) = this_matrix(scope, args.this()) else { return };
    let i = MATRIX_MEMBERS.iter().position(|n| *n == name).unwrap_or(0);
    rv.set_double(m[i]);
}

fn cb_matrix_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    if this_matrix(scope, args.this()).is_none() {
        return;
    }
    let Some(n) = args.get(0).number_value(scope) else { return };
    if !n.is_finite() {
        throw_type_error(scope, &format!("Failed to set the '{name}' property on 'SVGMatrix': The provided double value is non-finite."));
        return;
    }
    // a transform's `matrix` writes its transform (which becomes that `matrix`)
    if crate::core::dom::svg_values::transform_matrix_write(scope, args.this(), &name, n) {
        return;
    }
    crate::core::dom::svg_values::set_detached(scope, args.this(), &name, n);
}

/// The numeric arguments of a matrix method, each required and finite, as Chrome checks them.
fn finite_args(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, method: &str, n: usize) -> Option<Vec<f64>> {
    if (args.length() as usize) < n {
        throw_type_error(scope, &format!("Failed to execute '{method}' on 'SVGMatrix': {n} argument{} required, but only {} present.", if n == 1 { "" } else { "s" }, args.length()));
        return None;
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let v = args.get(i as i32).number_value(scope).unwrap_or(f64::NAN);
        if !v.is_finite() {
            throw_type_error(scope, &format!("Failed to execute '{method}' on 'SVGMatrix': The provided double value is non-finite."));
            return None;
        }
        out.push(v);
    }
    Some(out)
}

fn cb_matrix_op(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let op = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let Some(m) = this_matrix(scope, args.this()) else { return };
    let out = match op.as_str() {
        "inverse" => match inverse(m) {
            Some(i) => i,
            None => {
                crate::binding::protos::throw_dom(scope, "Failed to execute 'inverse' on 'SVGMatrix': The matrix is not invertible.", "InvalidStateError");
                return;
            }
        },
        "multiply" => {
            let other = args.get(0);
            let Some(o) = v8::Local::<v8::Object>::try_from(other).ok().and_then(|o| {
                crate::core::dom::svg_values::detached_nums(scope, o, &MATRIX_MEMBERS, 0.0)
            }) else {
                throw_type_error(scope, "Failed to execute 'multiply' on 'SVGMatrix': parameter 1 is not of type 'SVGMatrix'.");
                return;
            };
            mul(m, [o[0], o[1], o[2], o[3], o[4], o[5]])
        }
        "translate" => {
            let Some(a) = finite_args(scope, &args, "translate", 2) else { return };
            mul(m, [1.0, 0.0, 0.0, 1.0, a[0], a[1]])
        }
        "scale" => {
            let Some(a) = finite_args(scope, &args, "scale", 1) else { return };
            mul(m, [a[0], 0.0, 0.0, a[0], 0.0, 0.0])
        }
        "scaleNonUniform" => {
            let Some(a) = finite_args(scope, &args, "scaleNonUniform", 2) else { return };
            mul(m, [a[0], 0.0, 0.0, a[1], 0.0, 0.0])
        }
        "rotate" => {
            let Some(a) = finite_args(scope, &args, "rotate", 1) else { return };
            let r = a[0].to_radians();
            mul(m, [r.cos(), r.sin(), -r.sin(), r.cos(), 0.0, 0.0])
        }
        "flipX" => mul(m, [-1.0, 0.0, 0.0, 1.0, 0.0, 0.0]),
        "flipY" => mul(m, [1.0, 0.0, 0.0, -1.0, 0.0, 0.0]),
        "skewX" => {
            let Some(a) = finite_args(scope, &args, "skewX", 1) else { return };
            mul(m, [1.0, 0.0, a[0].to_radians().tan(), 1.0, 0.0, 0.0])
        }
        "skewY" => {
            let Some(a) = finite_args(scope, &args, "skewY", 1) else { return };
            mul(m, [1.0, a[0].to_radians().tan(), 0.0, 1.0, 0.0, 0.0])
        }
        _ => return,
    };
    if let Some(o) = matrix_value(scope, out) {
        rv.set(o.into());
    }
}

/// `SVGPoint.matrixTransform(matrix)`: a new detached point.
fn cb_point_matrix_transform(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(p) = crate::core::dom::svg_values::detached_nums(scope, args.this(), &["x", "y"], 0.0) else {
        throw_type_error(scope, "Illegal invocation");
        return;
    };
    let Some(m) = v8::Local::<v8::Object>::try_from(args.get(0)).ok().and_then(|o| {
        crate::core::dom::svg_values::detached_nums(scope, o, &MATRIX_MEMBERS, 0.0)
    }) else {
        throw_type_error(scope, "Failed to execute 'matrixTransform' on 'SVGPoint': parameter 1 is not of type 'DOMMatrixInit'.");
        return;
    };
    let (x, y) = (m[0] * p[0] + m[2] * p[1] + m[4], m[1] * p[0] + m[3] * p[1] + m[5]);
    if let Some(o) = crate::core::dom::svg_values::detached_value(scope, "SVGPoint", &[("x", x), ("y", y)]) {
        rv.set(o.into());
    }
}

/// [`method_row`] with the member's NAME as the callback's data, so one body serves several methods.
fn method_row_named<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    proto: v8::Local<v8::Object>,
    member: &'static MemberDef,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
) {
    use crate::binding::protos::{build_fn_data, make_arity_guard};
    let data = v8::String::new(scope, member.name).unwrap().into();
    let f = build_fn_data(scope, cb, member.name, member.length as i32, data);
    let v: v8::Local<v8::Value> = if member.required > 0 {
        make_arity_guard(scope, "SVGMatrix", member.name, member.required as i32, member.length as i32, f.into(), member.ty).into()
    } else {
        f.into()
    };
    crate::install::kits::define_value(scope, proto, member, v, true);
}

pub fn install_svg_geometry(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "getBBox" => method_row(scope, proto, member, cb_get_bbox),
        "getTotalLength" => method_row(scope, proto, member, cb_get_total_length),
        "getPointAtLength" => method_row(scope, proto, member, cb_get_point_at_length),
        "ownerSVGElement" => getter_row(scope, proto, member, cb_owner_svg),
        "pauseAnimations" => method_row(scope, proto, member, cb_pause),
        "unpauseAnimations" => method_row(scope, proto, member, cb_unpause),
        "animationsPaused" => method_row(scope, proto, member, cb_paused),
        "getCTM" => method_row(scope, proto, member, cb_get_ctm),
        "getScreenCTM" => method_row(scope, proto, member, cb_get_screen_ctm),
        "createSVGMatrix" => method_row(scope, proto, member, cb_create_svg_matrix),
        "getNumberOfChars" => method_row(scope, proto, member, crate::core::dom::svg_text::cb_get_number_of_chars),
        "getComputedTextLength" => method_row(scope, proto, member, crate::core::dom::svg_text::cb_get_computed_text_length),
        "getSubStringLength" => method_row(scope, proto, member, crate::core::dom::svg_text::cb_get_sub_string_length),
        "getStartPositionOfChar" => method_row(scope, proto, member, crate::core::dom::svg_text::cb_get_start_position_of_char),
        "getEndPositionOfChar" => method_row(scope, proto, member, crate::core::dom::svg_text::cb_get_end_position_of_char),
        "getExtentOfChar" => method_row(scope, proto, member, crate::core::dom::svg_text::cb_get_extent_of_char),
        "getRotationOfChar" => method_row(scope, proto, member, crate::core::dom::svg_text::cb_get_rotation_of_char),
        "getCharNumAtPosition" => method_row(scope, proto, member, crate::core::dom::svg_text::cb_get_char_num_at_position),
        "selectSubString" => method_row(scope, proto, member, crate::core::dom::svg_text::cb_select_sub_string),
        "matrixTransform" => method_row(scope, proto, member, cb_point_matrix_transform),
        "a" | "b" | "c" | "d" | "e" | "f" => {
            let g = v8::String::new(scope, member.name).unwrap().into();
            let sd = v8::String::new(scope, member.name).unwrap().into();
            crate::core::dom::shared::row_accessor_data(scope, proto, member, cb_matrix_get, g, cb_matrix_set, sd)
        }
        "inverse" | "multiply" | "translate" | "scale" | "scaleNonUniform" | "rotate" | "flipX" | "flipY" | "skewX" | "skewY" => {
            method_row_named(scope, proto, member, cb_matrix_op)
        }
        other => debug_assert!(false, "install_svg_geometry: unexpected member {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_length_and_points() {
        let lines = path_polylines("M0 0 L100 0 L100 100");
        let len: f64 = lines.iter().map(|p| p.windows(2).map(|w| seg_len(w[0], w[1])).sum::<f64>()).sum();
        assert_eq!(len, 200.0);
        let rel = path_polylines("m10 10 h20 v20 z");
        assert_eq!(rel[0], vec![(10.0, 10.0), (30.0, 10.0), (30.0, 30.0), (10.0, 10.0)]);
        assert_eq!(numbers("1-2.5.5e1,3"), vec![1.0, -2.5, 5.0, 3.0]);
    }
}
