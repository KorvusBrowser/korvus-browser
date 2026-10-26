//! A page's web fonts: the `@font-face` files it downloaded, drawn as its text.
//!
//! The document's painter names each face it chose by the family name [`register`] hands back -- a name no
//! installed font has (`korvus\u{1}wf3`), put in the `font-family` list ahead of what the page listed after it --
//! and [`crate::text::shape`] resolves such a name here instead of asking the font service. The registry is the
//! process's, keyed by the font's URL: the page 2D raster runs on whichever thread renders, so the bytes are
//! shared and each thread makes its own typeface from them.
//!
//! A variable font is drawn at the weight and size its text asks for: the painter names an instance,
//! `<family>@<weight>@<size>` (`korvus\u{1}wf3@700@64`), and its `wght` and `opsz` axes are set there (clamped to
//! them) -- Chrome sets `wght` from the used `font-weight` and `opsz` from the font size (`font-optical-sizing: auto`),
//! so one file serves a page's every weight and size. A font without an axis draws as itself along it.
//!
//! A WOFF or WOFF2 body is decoded back to its sfnt first (`wuff`: WOFF2's transformed `glyf`/`loca` and `hmtx`
//! included), so Skia, skrifa and HarfBuzz read the same file Chrome's OTS hands its font manager.

use skia_safe::{FontMgr, Typeface};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

/// The prefix of a registered font's family name: no family name in a stylesheet contains U+0001.
const PREFIX: &str = "korvus\u{1}wf";

/// How many bytes of decoded fonts the registry holds before it lets the oldest go.
const KEEP_BYTES: usize = 192 << 20;

#[derive(Default)]
struct Registry {
    /// URL -> its family name, and the length of the body it was made from (a URL refetched with another body
    /// is registered again).
    by_url: HashMap<String, (String, usize)>,
    /// Family name -> the sfnt.
    by_name: HashMap<String, Arc<Vec<u8>>>,
    order: VecDeque<(String, String)>,
    bytes: usize,
    next: u64,
}

static REGISTRY: Mutex<Option<Registry>> = Mutex::new(None);

thread_local! {
    /// Family name -> this thread's typeface for it, or none when Skia refused the file.
    static FACES: RefCell<HashMap<String, Option<Typeface>>> = RefCell::new(HashMap::new());
}

/// The sfnt in a font body: a WOFF or WOFF2 decoded, an sfnt or collection as it is. `None`: not a font.
fn sfnt(body: &[u8]) -> Option<Vec<u8>> {
    let out = match body.get(..4)? {
        b"wOFF" => wuff::decompress_woff1(body).ok()?,
        b"wOF2" => wuff::decompress_woff2(body).ok()?,
        _ => body.to_vec(),
    };
    skrifa::FontRef::from_index(&out, 0).ok()?;
    Some(out)
}

/// The family name the font downloaded from `url` draws under, registering `body` the first time. `None`: the
/// body is no font.
pub(crate) fn register(url: &str, body: &[u8]) -> Option<String> {
    let mut guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    let reg = guard.get_or_insert_with(Registry::default);
    if let Some((name, len)) = reg.by_url.get(url) {
        if *len == body.len() && reg.by_name.contains_key(name) {
            return Some(name.clone());
        }
    }
    let data = sfnt(body)?;
    let name = format!("{PREFIX}{}", reg.next);
    reg.next += 1;
    reg.bytes += data.len();
    reg.by_url.insert(url.to_string(), (name.clone(), body.len()));
    reg.by_name.insert(name.clone(), Arc::new(data));
    reg.order.push_back((url.to_string(), name.clone()));
    while reg.bytes > KEEP_BYTES && reg.order.len() > 1 {
        let Some((u, n)) = reg.order.pop_front() else { break };
        if let Some(d) = reg.by_name.remove(&n) {
            reg.bytes -= d.len();
        }
        if reg.by_url.get(&u).is_some_and(|(m, _)| *m == n) {
            reg.by_url.remove(&u);
        }
    }
    Some(name)
}

/// Is `family` a registered font's name (see the module doc)?
pub(crate) fn is_web_family(family: &str) -> bool {
    family.starts_with(PREFIX)
}

/// A registered family name, and the weight and optical size its instance is drawn at (see the module doc).
fn instance(family: &str) -> (&str, Option<f32>, Option<f32>) {
    let mut parts = family.split('@');
    let base = parts.next().unwrap_or(family);
    let weight = parts.next().and_then(|w| w.parse::<f32>().ok());
    let opsz = parts.next().and_then(|o| o.parse::<f32>().ok());
    (base, weight, opsz)
}

/// The registered font `name`'s file.
fn file(name: &str) -> Option<Arc<Vec<u8>>> {
    REGISTRY.lock().unwrap_or_else(|e| e.into_inner()).as_ref()?.by_name.get(name).cloned()
}

/// A font's axis `tag`, its least and greatest values; `None` for a font without one.
fn axis(data: &[u8], tag: &[u8; 4]) -> Option<(f32, f32)> {
    use skrifa::MetadataProvider;
    let font = skrifa::FontRef::from_index(data, 0).ok()?;
    font.axes().iter().find(|a| a.tag() == skrifa::Tag::new(tag)).map(|a| (a.min_value(), a.max_value()))
}

/// Where an instance sits on its font's axes: `wght` and `opsz` where the font has them, each clamped to its axis.
fn position(data: &[u8], weight: Option<f32>, opsz: Option<f32>) -> Vec<(&'static [u8; 4], f32)> {
    let mut out = Vec::new();
    if let (Some(w), Some((lo, hi))) = (weight, axis(data, b"wght")) {
        out.push((b"wght", w.clamp(lo, hi)));
    }
    if let (Some(o), Some((lo, hi))) = (opsz, axis(data, b"opsz")) {
        out.push((b"opsz", o.clamp(lo, hi)));
    }
    out
}

/// This thread's typeface for a registered font's family name, or an instance of it. `None`: not such a name, or
/// the font is gone.
pub(crate) fn typeface(mgr: &FontMgr, family: &str) -> Option<Typeface> {
    if !is_web_family(family) {
        return None;
    }
    if let Some(hit) = FACES.with(|t| t.borrow().get(family).cloned()) {
        return hit;
    }
    let (base, weight, opsz) = instance(family);
    let tf = if weight.is_none() && opsz.is_none() {
        file(base).and_then(|data| mgr.new_from_data(&data, None))
    } else {
        let data = file(base)?;
        let at = position(&data, weight, opsz);
        if at.is_empty() {
            // a font without the axes is the same typeface at every weight and size
            typeface(mgr, base)
        } else if opsz.is_some() && at.len() == 1 && at[0].0 == b"wght" {
            // no optical size to set: the weight's own instance, shared by every size
            typeface(mgr, &format!("{base}@{}", weight.unwrap_or(400.0)))
        } else {
            use skia_safe::font_arguments::{variation_position::Coordinate, VariationPosition};
            let coordinates: Vec<Coordinate> = at.iter().map(|(tag, value)| Coordinate { axis: skia_safe::FourByteTag::from_chars(tag[0] as char, tag[1] as char, tag[2] as char, tag[3] as char), value: *value }).collect();
            let args = skia_safe::FontArguments::new().set_variation_design_position(VariationPosition { coordinates: &coordinates });
            let plain = typeface(mgr, base);
            plain.as_ref().and_then(|t| t.clone_with_arguments(&args)).or(plain)
        }
    };
    FACES.with(|t| t.borrow_mut().insert(family.to_string(), tf.clone()));
    tf
}

/// The metrics layout measures text in the registered font `family`, or an instance of it, with (see
/// [`spec::FontFileMetrics`]).
pub(crate) fn metrics(family: &str) -> Option<spec::FontFileMetrics> {
    if !is_web_family(family) {
        return None;
    }
    let (base, weight, opsz) = instance(family);
    let data = file(base)?;
    let mut m = file_metrics(&data, &position(&data, weight, opsz))?;
    m.weight_axis = axis(&data, b"wght");
    m.opsz_axis = axis(&data, b"opsz");
    Some(m)
}

/// The cyclic order of `0..k` in which every ordered pair is adjacent exactly once (a de Bruijn sequence of order 2),
/// closed with its first element: shaped once, it holds every pair's adjustment.
fn every_pair(k: usize) -> Vec<usize> {
    fn walk(t: usize, p: usize, k: usize, a: &mut [usize; 3], out: &mut Vec<usize>) {
        if t > 2 {
            if 2 % p == 0 {
                out.extend_from_slice(&a[1..=p]);
            }
            return;
        }
        a[t] = a[t - p];
        walk(t + 1, p, k, a, out);
        for j in a[t - p] + 1..k {
            a[t] = j;
            walk(t + 1, t, k, a, out);
        }
    }
    let mut out = Vec::with_capacity(k * k + 1);
    walk(1, 1, k, &mut [0; 3], &mut out);
    if let Some(&first) = out.first() {
        out.push(first);
    }
    out
}

/// A font's metrics, at a place on its axes when it is given (a variable font's instance: [`position`]).
fn file_metrics(data: &[u8], place: &[(&[u8; 4], f32)]) -> Option<spec::FontFileMetrics> {
    use skrifa::instance::{LocationRef, Size};
    use skrifa::{MetadataProvider, string::StringId};
    let font = skrifa::FontRef::from_index(data, 0).ok()?;
    let location = (!place.is_empty()).then(|| font.axes().location(place.iter().map(|(tag, v)| (skrifa::Tag::new(tag), *v))));
    let at = location.as_ref().map(LocationRef::from).unwrap_or_default();
    let m = font.metrics(Size::unscaled(), at);
    let upem = m.units_per_em.max(1) as f32;
    let em = |v: f32| v / upem;
    let cmap = font.charmap();
    let glyphs = font.glyph_metrics(Size::unscaled(), at);
    let advance = |c: char| -> Option<f32> { cmap.map(c).and_then(|g| glyphs.advance_width(g)).map(em) };
    let (first, last) = (0x20u32, 0x7Eu32);
    let mut advances = Vec::with_capacity((last - first + 1) as usize);
    let mut covered = 0u128;
    for (i, code) in (first..=last).enumerate() {
        let a = char::from_u32(code).and_then(|c| cmap.map(c).map(|g| (g, c))).and_then(|(g, _)| glyphs.advance_width(g)).map(em);
        if a.is_some() {
            covered |= 1u128 << i;
        }
        advances.push(a.unwrap_or(0.0));
    }
    // Every printable-ASCII pair's adjustment from ONE shaping of a string in which each pair is adjacent once:
    // the first glyph's advance as HarfBuzz positions it, less its own. A pair a ligature touches (`fi`) is left out.
    let mut kerning = Vec::new();
    if let Some(mut face) = rustybuzz::Face::from_slice(data, 0) {
        for (tag, v) in place {
            face.set_variation(rustybuzz::ttf_parser::Tag::from_bytes(tag), *v);
        }
        let order = every_pair((last - first + 1) as usize);
        let text: String = order.iter().map(|&i| char::from_u32(first + i as u32).unwrap_or(' ')).collect();
        let mut buf = rustybuzz::UnicodeBuffer::new();
        buf.push_str(&text);
        buf.set_script(rustybuzz::script::LATIN);
        buf.set_direction(rustybuzz::Direction::LeftToRight);
        let shaped = rustybuzz::shape(&face, &[], buf);
        let infos = shaped.glyph_infos();
        let positions = shaped.glyph_positions();
        // each character's own glyph: the first whose cluster starts at it (ASCII: a character per byte)
        let mut glyph_of = vec![usize::MAX; order.len()];
        for (j, info) in infos.iter().enumerate() {
            if let Some(slot) = glyph_of.get_mut(info.cluster as usize) {
                if *slot == usize::MAX {
                    *slot = j;
                }
            }
        }
        for i in 0..order.len() - 1 {
            let (a, b) = (order[i], order[i + 1]);
            let j = glyph_of[i];
            if j == usize::MAX || glyph_of[i + 1] != j + 1 || covered & (1u128 << a) == 0 || covered & (1u128 << b) == 0 {
                continue;
            }
            let own = face.glyph_hor_advance(rustybuzz::ttf_parser::GlyphId(infos[j].glyph_id as u16)).unwrap_or(0) as i32;
            let delta = positions[j].x_advance - own;
            if delta != 0 {
                kerning.push((char::from_u32(first + a as u32)?, char::from_u32(first + b as u32)?, delta as f32 / upem));
            }
        }
    }
    kerning.sort_by(|x, y| (x.0, x.1).cmp(&(y.0, y.1)));
    kerning.dedup_by(|x, y| x.0 == y.0 && x.1 == y.1);
    // The font's own characters past ASCII and before the scripts set at one advance (CJK and the rest).
    let mut extra: Vec<(char, f32)> = cmap
        .mappings()
        .filter(|(code, _)| (0xA0..0x3000).contains(code) && !(0x1100..=0x11FF).contains(code))
        .take(4096)
        .filter_map(|(code, g)| Some((char::from_u32(code)?, em(glyphs.advance_width(g)?))))
        .collect();
    extra.sort_by(|x, y| x.0.cmp(&y.0));
    let class_advances = ['\u{AC00}', '\u{1100}', '\u{4E00}', '\u{3042}', '\u{3001}', '\u{1F600}'].map(|c| advance(c).unwrap_or(0.0));
    let family = font
        .localized_strings(StringId::FAMILY_NAME)
        .english_or_first()
        .map(|s| s.chars().collect::<String>())
        .unwrap_or_default();
    Some(spec::FontFileMetrics {
        family,
        // skrifa's ascent is above the baseline, its descent below it (negative)
        ascent: em(m.ascent),
        descent: em(-m.descent),
        line_gap: em(m.leading),
        x_height: m.x_height.map(em).unwrap_or(0.5),
        cap_height: m.cap_height.map(em).unwrap_or(0.7),
        zero_advance: advance('0').unwrap_or(0.5),
        avg_char_width: m.average_width.map(em),
        max_char_width: m.bounds.map(|b| em(b.x_max - b.x_min)),
        advances,
        covered,
        kerning,
        extra,
        class_advances,
        weight_axis: None,
        opsz_axis: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_font_file_registers_once_and_draws_under_its_name() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/realweb/seed/fontface/");
        let mgr = FontMgr::new();
        // the same Noto Sans Lycian as an sfnt, a WOFF and a WOFF2: each draws 𐊀 (U+10280) from its own glyph
        let mut names = Vec::new();
        for file in ["f.ttf", "f.woff", "f.woff2"] {
            let body = std::fs::read(format!("{root}{file}")).expect("fixture font");
            let url = format!("https://fonts.test/{file}");
            let name = register(&url, &body).expect("a font");
            assert!(is_web_family(&name));
            assert_eq!(register(&url, &body).as_deref(), Some(name.as_str()));
            let tf = typeface(&mgr, &name).expect("a typeface");
            assert_ne!(tf.unichar_to_glyph(0x10280), 0, "{file}");
            names.push(name);
        }
        names.dedup();
        assert_eq!(names.len(), 3);
        // the file's own metrics: Noto Sans Lycian's 1069/293 units per 1000 and its space
        let m = metrics(&names[2]).expect("metrics");
        assert_eq!(m.family, "Noto Sans Lycian");
        assert!((m.ascent - 1.069).abs() < 1e-3 && (m.descent - 0.293).abs() < 1e-3, "{m:?}");
        assert_ne!(m.covered & 1, 0);
        assert!(m.advances[0] > 0.0);
        assert!(register("https://fonts.test/stub.woff2", &std::fs::read(format!("{root}stub.woff2")).unwrap()).is_none());
        assert!(typeface(&mgr, "Arial").is_none());
    }

    /// A variable font's instance is its `wght` axis at the weight it names, measured and drawn there: a bold
    /// instance is wider than a light one (Ubuntu Sans, 100 to 800; skipped where it is not installed).
    #[test]
    fn a_variable_font_draws_and_measures_at_its_weight() {
        let Ok(body) = std::fs::read("/usr/share/fonts/truetype/ubuntu/UbuntuSans[wdth,wght].ttf") else { return };
        let name = register("https://fonts.test/UbuntuSans.ttf", &body).expect("a font");
        let (light, bold) = (metrics(&format!("{name}@300")).expect("light"), metrics(&format!("{name}@700")).expect("bold"));
        assert_eq!(light.weight_axis, Some((100.0, 800.0)));
        let a = (b'a' - 0x20) as usize;
        assert!(bold.advances[a] > light.advances[a], "{} {}", bold.advances[a], light.advances[a]);
        // past the axis, the axis's end
        assert_eq!(metrics(&format!("{name}@950")).unwrap().advances, metrics(&format!("{name}@800")).unwrap().advances);
        let mgr = FontMgr::new();
        let width = |family: &str| skia_safe::Font::from_typeface(typeface(&mgr, family).expect("a typeface"), 100.0).measure_str("Hamburgefonstiv", None).0;
        assert!(width(&format!("{name}@700")) > width(&format!("{name}@300")) + 10.0);
        // a font without the axis is itself at every weight
        let fixed = register("https://fonts.test/f.ttf", &std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/realweb/seed/fontface/f.ttf")).unwrap()).unwrap();
        assert_eq!(metrics(&format!("{fixed}@700")).unwrap().advances, metrics(&fixed).unwrap().advances);
        assert_eq!(metrics(&fixed).unwrap().weight_axis, None);
    }

    #[test]
    fn every_pair_is_adjacent_once() {
        let k = 95;
        let order = every_pair(k);
        assert_eq!(order.len(), k * k + 1);
        let mut seen = std::collections::HashSet::new();
        for w in order.windows(2) {
            assert!(seen.insert((w[0], w[1])));
        }
        assert_eq!(seen.len(), k * k);
    }
}
