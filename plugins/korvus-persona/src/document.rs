//! The shared persona format (`shared/personas/FORMAT.md`, D16-D17): korvus's reader of a persona document, and how
//! korvus wears its device (goal 1020 step 3).
//!
//! A document is one JSON object -- `persona` (the version, 1), `name`, `device` (the machine), `build` (which
//! browser build it presents) and `session` (how a run wears it). The rules are the format's, which the persona
//! compiler enforces for the device: [`read`] refuses a document every reader must refuse, with the reason the corpus
//! (`shared/personas/fixtures/`) expects -- the `the_shared_corpus` test holds it to every fixture there.
//!
//! WEARING A DEVICE ([`wear`]). A bundle is the build: the API surface, the CSS, the codecs, the TLS fingerprint of
//! one Chrome on one OS. A document's device is laid over it -- its cores and memory instead of this machine's, its
//! screen instead of the capture's, its GPU strings and WebGL set instead of this machine's GPU -- so a page reads the
//! document's machine. What the device does not state stays the bundle's (the screen) or the host's (the hardware,
//! the GPU), as without a document.
use serde_json::{json, Map, Value};

/// The WebGL parameters a device section names (`dataset/webgl/`, FORMAT.md), with their GL enums (the WebGL IDL's).
const WEBGL_PARAMS: &[(&str, u32)] = &[
    ("ALIASED_LINE_WIDTH_RANGE", 0x846E),
    ("ALIASED_POINT_SIZE_RANGE", 0x846D),
    ("ALPHA_BITS", 0x0D55),
    ("BLUE_BITS", 0x0D54),
    ("DEPTH_BITS", 0x0D56),
    ("GREEN_BITS", 0x0D53),
    ("MAX_3D_TEXTURE_SIZE", 0x8073),
    ("MAX_ARRAY_TEXTURE_LAYERS", 0x88FF),
    ("MAX_COLOR_ATTACHMENTS", 0x8CDF),
    ("MAX_COMBINED_FRAGMENT_UNIFORM_COMPONENTS", 0x8A33),
    ("MAX_COMBINED_TEXTURE_IMAGE_UNITS", 0x8B4D),
    ("MAX_COMBINED_UNIFORM_BLOCKS", 0x8A2E),
    ("MAX_COMBINED_VERTEX_UNIFORM_COMPONENTS", 0x8A31),
    ("MAX_CUBE_MAP_TEXTURE_SIZE", 0x851C),
    ("MAX_DRAW_BUFFERS", 0x8824),
    ("MAX_ELEMENTS_INDICES", 0x80E9),
    ("MAX_ELEMENTS_VERTICES", 0x80E8),
    ("MAX_ELEMENT_INDEX", 0x8D6B),
    ("MAX_FRAGMENT_INPUT_COMPONENTS", 0x9125),
    ("MAX_FRAGMENT_UNIFORM_BLOCKS", 0x8A2D),
    ("MAX_FRAGMENT_UNIFORM_COMPONENTS", 0x8B49),
    ("MAX_FRAGMENT_UNIFORM_VECTORS", 0x8DFD),
    ("MAX_PROGRAM_TEXEL_OFFSET", 0x8905),
    ("MAX_RENDERBUFFER_SIZE", 0x84E8),
    ("MAX_SAMPLES", 0x8D57),
    ("MAX_SERVER_WAIT_TIMEOUT", 0x9111),
    ("MAX_TEXTURE_IMAGE_UNITS", 0x8872),
    ("MAX_TEXTURE_LOD_BIAS", 0x84FD),
    ("MAX_TEXTURE_SIZE", 0x0D33),
    ("MAX_TRANSFORM_FEEDBACK_INTERLEAVED_COMPONENTS", 0x8C8A),
    ("MAX_TRANSFORM_FEEDBACK_SEPARATE_ATTRIBS", 0x8C8B),
    ("MAX_TRANSFORM_FEEDBACK_SEPARATE_COMPONENTS", 0x8C80),
    ("MAX_UNIFORM_BLOCK_SIZE", 0x8A30),
    ("MAX_UNIFORM_BUFFER_BINDINGS", 0x8A2F),
    ("MAX_VARYING_COMPONENTS", 0x8B4B),
    ("MAX_VARYING_VECTORS", 0x8DFC),
    ("MAX_VERTEX_ATTRIBS", 0x8869),
    ("MAX_VERTEX_OUTPUT_COMPONENTS", 0x9122),
    ("MAX_VERTEX_TEXTURE_IMAGE_UNITS", 0x8B4C),
    ("MAX_VERTEX_UNIFORM_BLOCKS", 0x8A2B),
    ("MAX_VERTEX_UNIFORM_COMPONENTS", 0x8B4A),
    ("MAX_VERTEX_UNIFORM_VECTORS", 0x8DFB),
    ("MAX_VIEWPORT_DIMS", 0x0D3A),
    ("MIN_PROGRAM_TEXEL_OFFSET", 0x8904),
    ("RED_BITS", 0x0D52),
    ("RENDERER", 0x1F01),
    ("SAMPLES", 0x80A9),
    ("SAMPLE_BUFFERS", 0x80A8),
    ("SHADING_LANGUAGE_VERSION", 0x8B8C),
    ("STENCIL_BITS", 0x0D57),
    ("SUBPIXEL_BITS", 0x0D50),
    ("UNIFORM_BUFFER_OFFSET_ALIGNMENT", 0x8A34),
    ("VENDOR", 0x1F00),
    ("VERSION", 0x1F02),
];

/// A version-1 persona document, read and checked.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    pub name: String,
    pub device: Value,
    pub build: Option<Value>,
    pub session: Option<Value>,
}

impl Document {
    /// The device's OS (`windows`, `macos`, `linux`, `android`).
    pub fn os(&self) -> &str {
        self.device.get("os").and_then(Value::as_str).unwrap_or("")
    }
}

/// Read and check a document. `Err` holds every reason it is refused, in the reference validator's order
/// (`shared/personas/format/validate.py`).
pub fn read(text: &str) -> Result<Document, Vec<String>> {
    let v: Value = serde_json::from_str(text).map_err(|e| vec![format!("not JSON: {e}")])?;
    let errs = errors(&v);
    if !errs.is_empty() {
        return Err(errs);
    }
    Ok(Document {
        name: v["name"].as_str().unwrap_or_default().to_string(),
        device: v["device"].clone(),
        build: v.get("build").cloned(),
        session: v.get("session").cloned(),
    })
}

const TOP: &[&str] = &["persona", "name", "device", "build", "session"];
const DEVICE: &[&str] = &[
    "os", "os_version", "architecture", "bitness", "model", "brand", "hardware", "screen", "gpu", "webgl", "webgpu", "voices", "window",
    "noise", "fonts", "media_devices", "battery", "storage", "preferences", "audio",
];
const WINDOW: &[&str] = &["frame", "outer", "position"];
const BATTERY: &[&str] = &["charging", "level"];
const PREFERENCES: &[&str] = &["color_scheme", "reduced_motion"];
/// The kinds `enumerateDevices()` lists, in Chrome's order.
const MEDIA_KINDS: &[&str] = &["audioinput", "videoinput", "audiooutput"];
const HARDWARE: &[&str] = &["cores", "memory_gb"];
const SCREEN: &[&str] = &["width", "height", "avail_width", "avail_height", "avail_top", "avail_left", "dpr", "color_depth", "color_gamut", "hdr", "refresh_rate", "touch_points"];
const GPU: &[&str] = &["vendor", "renderer"];
const WEBGPU: &[&str] = &["vendor", "architecture", "device", "description", "limits"];
const WEBGL_VERSION: &[&str] = &["params", "extensions", "precision", "attributes", "max_anisotropy"];
const BUILD: &[&str] = &["browser", "milestone", "os", "bundle"];
const SESSION: &[&str] = &["proxy", "timezone", "locale", "humanize", "persistent", "plugins"];
/// The navigator.deviceMemory values Chrome reports on a desktop (Chrome's `kMemoryBuckets`).
const DESKTOP_MEMORY: &[f64] = &[2.0, 4.0, 8.0, 16.0, 32.0];

fn unknown(out: &mut Vec<String>, at: &str, given: &Map<String, Value>, known: &[&str]) {
    let mut keys: Vec<&String> = given.keys().filter(|k| !k.starts_with('_') && !known.contains(&k.as_str())).collect();
    keys.sort();
    for k in keys {
        out.push(format!("unknown field {at}{k}"));
    }
}

fn int(v: Option<&Value>) -> Option<i64> {
    v.and_then(|x| if x.is_i64() || x.is_u64() { x.as_i64() } else { None })
}

fn num(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64)
}

/// The reasons `v` is not a version-1 persona document, in the reference validator's order; empty when it is one.
pub fn errors(v: &Value) -> Vec<String> {
    let Some(doc) = v.as_object() else { return vec!["a persona document is a JSON object".into()] };
    let mut out = Vec::new();
    if int(doc.get("persona")) != Some(1) {
        out.push("\"persona\" must be 1 (this format's version)".into());
    }
    if !doc.get("name").and_then(Value::as_str).is_some_and(|n| !n.is_empty()) {
        out.push("name must be a non-empty string".into());
    }
    unknown(&mut out, "", doc, TOP);
    let Some(device) = doc.get("device").and_then(Value::as_object) else {
        out.push("device is required (an object)".into());
        return out;
    };
    unknown(&mut out, "device.", device, DEVICE);
    for (key, known) in [("hardware", HARDWARE), ("screen", SCREEN), ("gpu", GPU), ("webgpu", WEBGPU)] {
        match device.get(key) {
            Some(Value::Object(m)) => unknown(&mut out, &format!("device.{key}."), m, known),
            Some(_) => out.push(format!("device.{key} is an object")),
            None => {}
        }
    }
    compiler_errors(&mut out, device);
    if device.get("brand").is_some_and(|b| b != "chrome") {
        out.push("device.brand is chrome".into());
    }
    let os = device.get("os").and_then(Value::as_str).unwrap_or("");
    if device.contains_key("model") && os != "android" {
        out.push("device.model is a phone's (android only)".into());
    }
    match device.get("webgl") {
        None => {}
        Some(Value::Object(webgl)) => {
            unknown(&mut out, "device.webgl.", webgl, &["webgl1", "webgl2"]);
            for version in ["webgl1", "webgl2"] {
                let Some(v) = webgl.get(version).and_then(Value::as_object) else { continue };
                unknown(&mut out, &format!("device.webgl.{version}."), v, WEBGL_VERSION);
                if let Some(params) = v.get("params").and_then(Value::as_object) {
                    let mut names: Vec<&String> = params.keys().collect();
                    names.sort();
                    for n in names {
                        if !WEBGL_PARAMS.iter().any(|(p, _)| p == n) {
                            out.push(format!("unknown WebGL parameter device.webgl.{version}.params.{n}"));
                        }
                    }
                }
                if let Some(prec) = v.get("precision").and_then(Value::as_object) {
                    let mut names: Vec<&String> = prec.keys().collect();
                    names.sort();
                    for n in names {
                        if precision_key(n).is_none() {
                            out.push(format!("unknown WebGL precision device.webgl.{version}.precision.{n}"));
                        }
                    }
                }
                if v.get("extensions").is_some_and(|e| !e.is_array()) {
                    out.push(format!("device.webgl.{version}.extensions is a list"));
                }
            }
        }
        Some(_) => out.push("device.webgl is an object".into()),
    }
    if let Some(voices) = device.get("voices") {
        let ok = voices.as_array().is_some_and(|a| a.iter().all(|x| x.get("name").is_some_and(Value::is_string) && x.get("lang").is_some_and(Value::is_string)));
        if !ok {
            out.push("device.voices is [{\"name\", \"lang\"}]".into());
        }
    }
    match doc.get("build") {
        None => {}
        Some(Value::Object(b)) => {
            unknown(&mut out, "build.", b, BUILD);
            if b.get("browser").and_then(Value::as_str) != Some("chrome") {
                out.push("build.browser is chrome".into());
            }
            if b.contains_key("milestone") && !int(b.get("milestone")).is_some_and(|m| m > 0) {
                out.push("build.milestone is a positive integer".into());
            }
            if b.contains_key("os") && b.get("os").and_then(Value::as_str) != Some(os) {
                out.push("build.os is the device's os".into());
            }
        }
        Some(_) => out.push("build is an object".into()),
    }
    match doc.get("session") {
        None => {}
        Some(Value::Object(s)) => {
            unknown(&mut out, "session.", s, SESSION);
            if s.get("proxy").is_some_and(|p| !p.as_str().is_some_and(proxy_url)) {
                out.push("session.proxy is an http, https or socks5 URL".into());
            }
            if s.get("timezone").is_some_and(|t| !t.as_str().is_some_and(iana_zone)) {
                out.push("session.timezone is an IANA zone (Area/Location)".into());
            }
            if s.get("locale").is_some_and(|l| !l.as_str().is_some_and(bcp47)) {
                out.push("session.locale is a BCP 47 tag (ko-KR)".into());
            }
            for flag in ["humanize", "persistent"] {
                if s.get(flag).is_some_and(|f| !f.is_boolean()) {
                    out.push(format!("session.{flag} is a boolean"));
                }
            }
            if s.get("plugins").is_some_and(|p| !p.as_array().is_some_and(|a| a.iter().all(|x| x.as_str().is_some_and(|s| !s.is_empty())))) {
                out.push("session.plugins is a list of paths".into());
            }
        }
        Some(_) => out.push("session is an object".into()),
    }
    out
}

/// The persona compiler's checks of a device (mirrored by the persona format's validator, `dataset/validate.py`).
fn compiler_errors(out: &mut Vec<String>, device: &Map<String, Value>) {
    let os = device.get("os").and_then(Value::as_str).unwrap_or("");
    if !["windows", "macos", "linux", "android"].contains(&os) {
        out.push("os must be windows, macos, linux or android".into());
        return;
    }
    if let Some(a) = device.get("architecture") {
        if !matches!(a.as_str(), Some("x86") | Some("arm")) {
            out.push("architecture must be x86 or arm".into());
        }
    }
    if let Some(b) = device.get("bitness") {
        if !matches!(b.as_str(), Some("64") | Some("32")) {
            out.push("bitness must be 64 or 32".into());
        }
    }
    if let Some(hw) = device.get("hardware") {
        if !int(hw.get("cores")).is_some_and(|c| (1..=256).contains(&c)) {
            out.push("hardware.cores must be 1-256".into());
        }
        if !num(hw.get("memory_gb")).is_some_and(|m| DESKTOP_MEMORY.contains(&m)) {
            out.push("hardware.memory_gb must be what Chrome 155 reports on a desktop: 2, 4, 8, 16 or 32".into());
        }
    }
    if let Some(screen) = device.get("screen") {
        match (int(screen.get("width")), int(screen.get("height"))) {
            (Some(w), Some(h)) if w > 0 && h > 0 => {
                let aw = int(screen.get("avail_width")).unwrap_or(w);
                let ah = int(screen.get("avail_height")).unwrap_or(h);
                if aw <= 0 || ah <= 0 || aw > w || ah > h {
                    out.push("the screen's available area must fit inside it".into());
                }
                let top = int(screen.get("avail_top")).unwrap_or(if os == "macos" { h - ah } else { 0 });
                if top < 0 || top + ah > h {
                    out.push("screen.avail_top leaves the available area inside the screen".into());
                }
            }
            _ => out.push("screen needs a positive width and height".into()),
        }
        if screen.get("touch_points").is_some() && os != "android" {
            out.push("screen.touch_points is a phone's: 5, 2 or 1".into());
        }
        if !matches!(int(screen.get("color_depth")).unwrap_or(24), 24 | 30) {
            out.push("screen.color_depth is 24 or 30".into());
        }
        if !matches!(screen.get("color_gamut").and_then(Value::as_str).unwrap_or("srgb"), "srgb" | "p3" | "rec2020") {
            out.push("screen.color_gamut is \"srgb\", \"p3\" or \"rec2020\"".into());
        }
        if !(24.0..=500.0).contains(&num(screen.get("refresh_rate")).unwrap_or(60.0)) {
            out.push("screen.refresh_rate is 24-500 (Hz)".into());
        }
        if let Some(dpr) = num(screen.get("dpr")) {
            if !(0.5..=4.0).contains(&dpr) {
                out.push("screen.dpr must be 0.5-4".into());
            }
        }
    }
    if let Some(gpu) = device.get("gpu") {
        let vendor = gpu.get("vendor").and_then(Value::as_str).unwrap_or("");
        let renderer = gpu.get("renderer").and_then(Value::as_str).unwrap_or("");
        let backends: Option<(&str, &str)> = match os {
            "windows" => Some(("Direct3D11", "D3D11")),
            "macos" => Some(("Metal", "OpenGL")),
            "linux" => Some(("OpenGL", "Vulkan")),
            _ => None,
        };
        if vendor.is_empty() || renderer.is_empty() {
            out.push("gpu needs a vendor and a renderer".into());
        } else if let Some((a, b)) = backends {
            if !renderer.contains(a) && !renderer.contains(b) {
                out.push(format!("a {os} GPU renderer names {a} (or {b}): {renderer}"));
            }
        }
    }
    if let Some(noise) = device.get("noise") {
        // `text` (ABI-3 KORVUS_READBACK_TEXT, the measureText/TextMetrics font-box tap) is a legal kind korvus taps;
        // other engines accept-and-ignore it (decisions.md D94). The refusal message still names only the three the
        // shared corpus asserts, so a genuinely unknown kind reads the same message the fixtures expect.
        if noise.as_array().is_some_and(|a| a.iter().any(|k| !matches!(k.as_str(), Some("canvas") | Some("audio") | Some("webgl") | Some("text")))) {
            out.push("noise kinds are canvas, audio and webgl".into());
        }
    }
    if let Some(window) = device.get("window") {
        if let Some(w) = window.as_object() {
            unknown(out, "device.window.", w, WINDOW);
        }
        let pair = |k: &str, ok: &dyn Fn(i64) -> bool| window.get(k).is_none_or(|f| f.as_array().is_some_and(|f| f.len() == 2 && f.iter().all(|v| int(Some(v)).is_some_and(ok))));
        if !window.is_object() || window.get("frame").is_none() && window.get("outer").is_none() || !pair("frame", &|n| (0..=400).contains(&n)) {
            out.push("window.frame is [outerWidth - innerWidth, outerHeight - innerHeight], each 0-400".into());
        }
        if !pair("outer", &|n| n > 0) {
            out.push("window.outer is [outerWidth, outerHeight], positive".into());
        }
        if !pair("position", &|n| n.abs() < 100_000) {
            out.push("window.position is [screenX, screenY]".into());
        }
    }
    if let Some(f) = device.get("fonts") {
        if !f.as_array().is_some_and(|a| a.iter().all(|x| x.as_str().is_some_and(|s| !s.trim().is_empty()))) {
            out.push("device.fonts is a list of family names".into());
        }
    }
    if let Some(m) = device.get("media_devices") {
        let ok = m.as_array().is_some_and(|a| {
            let kinds: Vec<&str> = a.iter().filter_map(Value::as_str).collect();
            kinds.len() == a.len() && kinds.iter().all(|k| MEDIA_KINDS.contains(k)) && kinds.windows(2).all(|w| MEDIA_KINDS.iter().position(|k| *k == w[0]) < MEDIA_KINDS.iter().position(|k| *k == w[1]))
        });
        if !ok {
            out.push("device.media_devices lists audioinput, videoinput, audiooutput, each once, in that order".into());
        }
    }
    if let Some(b) = device.get("battery") {
        if let Some(o) = b.as_object() {
            unknown(out, "device.battery.", o, BATTERY);
        }
        if !b.get("charging").is_some_and(Value::is_boolean) || !num(b.get("level")).is_some_and(|l| (0.0..=1.0).contains(&l)) {
            out.push("device.battery is {charging: boolean, level: 0-1}".into());
        }
    }
    if let Some(st) = device.get("storage") {
        if !num(st.get("quota")).is_some_and(|q| q > 0.0) {
            out.push("device.storage.quota is a positive number of bytes".into());
        }
    }
    if let Some(p) = device.get("preferences") {
        if let Some(o) = p.as_object() {
            unknown(out, "device.preferences.", o, PREFERENCES);
        }
        if p.get("color_scheme").is_some_and(|c| !matches!(c.as_str(), Some("light" | "dark"))) || p.get("reduced_motion").is_some_and(|r| !r.is_boolean()) {
            out.push("device.preferences is {color_scheme: light|dark, reduced_motion: boolean}".into());
        }
    }
    if let Some(a) = device.get("audio") {
        if !num(a.get("sample_rate")).is_some_and(|r| (8000.0..=192000.0).contains(&r)) {
            out.push("device.audio.sample_rate is 8000-192000 (Hz)".into());
        }
    }
}

fn proxy_url(s: &str) -> bool {
    let Some(rest) = ["http://", "https://", "socks5://"].iter().find_map(|p| s.strip_prefix(p)) else { return false };
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    !rest.is_empty() && !rest.contains('/') && !rest.chars().any(char::is_whitespace)
}

fn iana_zone(s: &str) -> bool {
    if s == "UTC" {
        return true;
    }
    let mut parts = s.split('/');
    let area = parts.next().unwrap_or("");
    let rest: Vec<&str> = parts.collect();
    area.len() >= 2
        && area.starts_with(|c: char| c.is_ascii_uppercase())
        && area.chars().all(|c| c.is_ascii_alphabetic() || c == '_')
        && !rest.is_empty()
        && rest.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || "_+-".contains(c)))
}

fn bcp47(s: &str) -> bool {
    let mut parts = s.split('-');
    let lang = parts.next().unwrap_or("");
    (2..=3).contains(&lang.len())
        && lang.chars().all(|c| c.is_ascii_lowercase())
        && parts.all(|p| (2..=8).contains(&p.len()) && p.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// A device's precision name (`VERTEX_LOW_FLOAT`) as the device oracle keys it (`VERTEX_SHADER.LOW_FLOAT`).
fn precision_key(name: &str) -> Option<String> {
    let (shader, rest) = name.split_once('_')?;
    if !matches!(shader, "VERTEX" | "FRAGMENT") {
        return None;
    }
    let (p, kind) = rest.split_once('_')?;
    if !matches!(p, "LOW" | "MEDIUM" | "HIGH") || !matches!(kind, "FLOAT" | "INT") {
        return None;
    }
    Some(format!("{shader}_SHADER.{p}_{kind}"))
}

// ── wearing a device ─────────────────────────────────────────────────────────────────────────────────

/// The device's cores and memory, when it states them.
pub fn hardware(doc: &Document) -> Option<(u32, f64)> {
    let hw = doc.device.get("hardware")?;
    Some((int(hw.get("cores"))? as u32, num(hw.get("memory_gb"))?))
}

/// The device's screen over the bundle's: its size, available area, scale and depth; the window maximized in the
/// available area with the bundle's own frame (or the device's `window.frame`), facing the way the screen does.
pub fn screen(doc: &Document, bundle: &spec::ScreenProfile) -> Option<spec::ScreenProfile> {
    let s = doc.device.get("screen")?;
    let (w, h) = (int(s.get("width"))? as u32, int(s.get("height"))? as u32);
    let aw = int(s.get("avail_width")).map(|v| v as u32).unwrap_or(w);
    let ah = int(s.get("avail_height")).map(|v| v as u32).unwrap_or(h);
    let top = int(s.get("avail_top")).map(|v| v as i32).unwrap_or(if doc.os() == "macos" { (h - ah) as i32 } else { 0 });
    let left = int(s.get("avail_left")).map(|v| v as i32).unwrap_or(0);
    let (fw, fh) = match doc.device.get("window").and_then(|w| w.get("frame")).and_then(Value::as_array) {
        Some(f) if f.len() == 2 => (int(f.first()).unwrap_or(0) as u32, int(f.get(1)).unwrap_or(0) as u32),
        _ => (
            bundle.window_outer_width.saturating_sub(bundle.window_inner_width),
            bundle.window_outer_height.saturating_sub(bundle.window_inner_height),
        ),
    };
    let mut out = bundle.clone();
    out.width = w;
    out.height = h;
    out.avail_width = aw;
    out.avail_height = ah;
    out.avail_top = top;
    out.avail_left = left;
    if let Some(dpr) = num(s.get("dpr")) {
        out.device_pixel_ratio = dpr;
    }
    if let Some(d) = int(s.get("color_depth")) {
        out.color_depth = d as u32;
    }
    if let Some(r) = num(s.get("refresh_rate")) {
        out.refresh_rate = r;
    }
    if let Some(g) = s.get("color_gamut").and_then(Value::as_str) {
        out.color_gamut = g.to_string();
    }
    if let Some(h) = s.get("hdr").and_then(Value::as_bool) {
        out.hdr = h;
    }
    // the window: where the device says, else maximized in the available area
    let pair = |k: &str| -> Option<(i64, i64)> {
        let a = doc.device.get("window")?.get(k)?.as_array()?;
        Some((int(a.first())?, int(a.get(1))?))
    };
    let (ow, oh) = pair("outer").map(|(w, h)| ((w as u32).min(aw), (h as u32).min(ah))).unwrap_or((aw, ah));
    // a restored (not maximized) window on Windows keeps DWM's invisible resize borders inside its outer size: 8 px
    // left, right and bottom, which a maximized one pushes off the screen (the bundle's frame is a maximized one's)
    let restored = (ow, oh) != (aw, ah) && doc.os() == "windows" && doc.device.get("window").and_then(|w| w.get("frame")).is_none();
    let (fw, fh) = if restored { (fw + 16, fh + 8) } else { (fw, fh) };
    let (x, y) = pair("position").map(|(x, y)| (x as i32, y as i32)).unwrap_or((left, top));
    out.window_outer_width = ow;
    out.window_outer_height = oh;
    out.window_inner_width = ow.saturating_sub(fw);
    out.window_inner_height = oh.saturating_sub(fh);
    out.window_screen_x = x;
    out.window_screen_y = y;
    let prefs = doc.device.get("preferences");
    out.prefers_dark = prefs.and_then(|p| p.get("color_scheme")).and_then(Value::as_str) == Some("dark");
    out.reduced_motion = prefs.and_then(|p| p.get("reduced_motion")).and_then(Value::as_bool) == Some(true);
    if !out.orientation_type.is_empty() {
        out.orientation_type = if w >= h { "landscape-primary".into() } else { "portrait-primary".into() };
        out.orientation_angle = 0;
    }
    Some(out)
}

/// The UA-CH `platformVersion` a device's `os_version` reports over the bundle's (`bundle`), as Chrome writes it (three
/// numbers: `10.0` is `10.0.0`), when the release can be worn on that build. The build's other surfaces -- its fonts
/// above all -- are the capture's OS release, so a device may only claim a release whose machine shows them:
///
/// * Windows: the same generation. `13.0.0` and up is Windows 11, below it Windows 10; a Windows 10 claim over a
///   Windows 11 capture lists fonts Windows 10 never shipped (and the reverse lacks ones every Windows 11 has).
/// * macOS: the capture's major or a later one -- a later macOS keeps every font an earlier one had; an earlier claim
///   would list fonts it did not have yet.
///
/// `None`: nothing to claim, or a release this build cannot be.
pub fn platform_version(os: &str, claimed: &str, bundle: &str) -> Option<String> {
    let nums = |v: &str| -> Option<Vec<u32>> { v.split('.').map(|p| p.trim().parse::<u32>().ok()).collect() };
    let mut c = nums(claimed).filter(|c| !c.is_empty() && c.len() <= 3)?;
    let b = nums(bundle).filter(|b| !b.is_empty())?;
    let fits = match os {
        "windows" => (c[0] >= 13) == (b[0] >= 13),
        "macos" => c[0] >= b[0],
        _ => false,
    };
    if !fits {
        return None;
    }
    c.resize(3, 0);
    Some(c.iter().map(u32::to_string).collect::<Vec<_>>().join("."))
}

/// The device's speech voices as `speechSynthesis.getVoices()` lists them: each its name and language, local, the first
/// the default, its `voiceURI` its name (as a local voice's is).
pub fn voices(doc: &Document) -> Option<Vec<spec::SpeechVoice>> {
    let list = doc.device.get("voices")?.as_array()?;
    Some(
        list.iter()
            .enumerate()
            .filter_map(|(i, v)| {
                let name = v.get("name")?.as_str()?.to_string();
                Some(spec::SpeechVoice { voice_uri: name.clone(), name, lang: v.get("lang")?.as_str()?.to_string(), local_service: true, default: i == 0 })
            })
            .collect(),
    )
}

/// The bundle's device oracle with the device's GPU strings and WebGL set in it, when the device states them; marked
/// `_document` so the engine does not put this machine's GPU in their place (`webgl.rs` `with_device`).
pub fn device_oracle(doc: &Document, bundle_oracle: &str) -> Option<String> {
    let gpu = doc.device.get("gpu");
    let webgl = doc.device.get("webgl");
    let rate = doc.device.get("audio").and_then(|a| num(a.get("sample_rate")));
    if gpu.is_none() && webgl.is_none() && rate.is_none() {
        return None;
    }
    let mut oracle: Value = serde_json::from_str(bundle_oracle).unwrap_or_else(|_| json!({}));
    if let Some(r) = rate {
        // the live `AudioContext`'s rate: the output device's
        if !oracle.get("audio").is_some_and(Value::is_object) {
            oracle["audio"] = json!({});
        }
        oracle["audio"]["sampleRate"] = json!(r);
    }
    if gpu.is_none() && webgl.is_none() {
        return Some(oracle.to_string());
    }
    if !oracle.get("webgl").is_some_and(Value::is_object) {
        oracle["webgl"] = json!({});
    }
    let block = oracle["webgl"].as_object_mut().expect("an object just made");
    if let Some(g) = gpu {
        let (vendor, renderer) = (g["vendor"].clone(), g["renderer"].clone());
        block.insert("unmaskedVendor".into(), vendor.clone());
        block.insert("unmaskedRenderer".into(), renderer.clone());
        for params in ["paramsByNum", "paramsByNumV2"] {
            if let Some(p) = block.get_mut(params).and_then(Value::as_object_mut) {
                p.insert("37445".into(), vendor.clone());
                p.insert("37446".into(), renderer.clone());
            }
        }
    }
    if let Some(w) = webgl {
        for (version, params_key, ext_key) in [("webgl1", "paramsByNum", "extensions"), ("webgl2", "paramsByNumV2", "extensionsV2")] {
            let Some(v) = w.get(version) else { continue };
            if let Some(params) = v.get("params").and_then(Value::as_object) {
                let target = block.entry(params_key).or_insert_with(|| json!({}));
                if let Some(t) = target.as_object_mut() {
                    for (name, value) in params {
                        if let Some((_, e)) = WEBGL_PARAMS.iter().find(|(p, _)| p == name) {
                            t.insert(e.to_string(), value.clone());
                        }
                    }
                    if let Some(a) = v.get("max_anisotropy") {
                        // EXT_texture_filter_anisotropic's MAX_TEXTURE_MAX_ANISOTROPY_EXT
                        t.insert("34047".into(), a.clone());
                    }
                }
            }
            if let Some(ext) = v.get("extensions") {
                block.insert(ext_key.into(), ext.clone());
            }
            if version == "webgl1" {
                if let Some(prec) = v.get("precision").and_then(Value::as_object) {
                    let mapped: Map<String, Value> = prec.iter().filter_map(|(k, val)| precision_key(k).map(|k| (k, val.clone()))).collect();
                    block.insert("precision".into(), Value::Object(mapped));
                }
                if let Some(attrs) = v.get("attributes") {
                    block.insert("contextAttributes".into(), attrs.clone());
                }
            }
        }
    }
    block.insert("_document".into(), json!(doc.name));
    Some(oracle.to_string())
}

/// Wear `doc`'s device over `base` (the build): its hardware, else this machine's; its screen, its GPU and WebGL
/// set, else the bundle's and this machine's; its session's zone and locale, else the build's.
pub fn wear(doc: &Document, base: crate::CapturedPersona, host: &spec::coherence::HostFacts) -> crate::Overridden {
    use spec::Persona;
    let bundle_screen = base.screen();
    let bundle_oracle = base.device_oracle_json();
    let bundle_platform_version = base.client_hints().platform_version;
    let bundle_voices = base.speech_voices();
    let bundle_adapter = base.bundle_dir().and_then(|d| std::fs::read_to_string(d.join("webgpu-adapter.json")).ok());
    let bundle_languages = base.navigator().languages;
    let mut o = crate::Overridden::on(base).hardware_of(host);
    if let Some((cores, memory)) = hardware(doc) {
        o = o.with_hardware(cores, memory);
    }
    if let Some(s) = screen(doc, &bundle_screen) {
        o = o.screen(s);
    }
    if let Some(oracle) = device_oracle(doc, &bundle_oracle) {
        o = o.with_device_oracle(oracle);
    }
    let text = |k: &str| doc.device.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let version = platform_version(doc.os(), &text("os_version"), &bundle_platform_version).unwrap_or_default();
    if !version.is_empty() || !text("architecture").is_empty() || !text("bitness").is_empty() {
        o = o.os_release(&version, &text("architecture"), &text("bitness"));
    }
    if let Some(kinds) = doc.device.get("media_devices").and_then(Value::as_array) {
        o = o.media_device_kinds(kinds.iter().filter_map(Value::as_str).map(String::from).collect());
    }
    if let Some(q) = doc.device.get("storage").and_then(|s| num(s.get("quota"))) {
        o = o.storage_quota(q.floor());
    }
    if let Some(b) = battery(doc) {
        o = o.battery(b);
    }
    if let Some(a) = webgpu_adapter(doc, bundle_adapter.as_deref()) {
        o = o.webgpu_adapter(a);
    }
    // the session's zone and locale (a proxy's exit place, as the identity found it), else the build's
    let session = |k: &str| doc.session.as_ref().and_then(|s| s.get(k)).and_then(Value::as_str).filter(|v| !v.is_empty());
    if let Some(tz) = session("timezone") {
        o = o.timezone(tz);
    }
    // an explicit locale (`--locale`, a context's `korvus.locale`: `$KORVUS_LOCALE`, as `apply_context` sets it) over
    // the session's: left to the session alone, `--locale en-US` moved ICU's locale (`engine::persona_env` reads the
    // variable) while `navigator.languages` and Accept-Language kept the capture's `ko-KR` -- one page, two answers
    let explicit = std::env::var("KORVUS_LOCALE").ok().map(|l| l.trim().to_string()).filter(|l| !l.is_empty());
    let tag = explicit.as_deref().or_else(|| session("locale")).map(str::to_string);
    if let Some(tag) = &tag {
        o = o.locale(tag);
    }
    // the device's language decides what a machine of it has installed: its voices and its supplemental fonts, and
    // its browser's standard font -- the bundle's are the capture's language's
    let lang = tag.clone().unwrap_or_else(|| bundle_languages.first().cloned().unwrap_or_default());
    match voices(doc) {
        Some(v) => o = o.voices(v.into_iter().chain(bundle_voices.iter().filter(|v| !v.local_service).cloned()).collect()),
        None if doc.os() == "windows" && tag.is_some() => {
            o = o.voices(windows_voices(&lang).into_iter().chain(bundle_voices.iter().filter(|v| !v.local_service).cloned()).collect());
        }
        None => {}
    }
    o.font_layer(font_layer(doc, &lang))
}

/// The device's fonts over the bundle's: the families it lists, else what its language installs. On Windows, a
/// language's supplemental fonts come with that language (Settings > Optional features: "Korean Supplemental Fonts"
/// -- Gulim, Dotum, Batang, Gungsuh -- for a Korean one), so a machine of another language has none of them; and the
/// browser's standard font is its UI language's (`locale_settings_win.grd`: Times New Roman for a Latin-script UI,
/// the bundle's own -- the capture's Korean UI's Malgun Gothic -- for Korean).
pub fn font_layer(doc: &Document, lang: &str) -> spec::FontLayer {
    let installed = doc.device.get("fonts").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect::<Vec<_>>());
    let primary = lang.split(['-', '_']).next().unwrap_or("").to_ascii_lowercase();
    let mut layer = spec::FontLayer { installed, ..Default::default() };
    if doc.os() == "windows" {
        if primary != "ko" && !primary.is_empty() {
            layer.absent = KOREAN_SUPPLEMENTAL.iter().map(|f| f.to_string()).collect();
        }
        // a CJK UI's standard font is its script's (not modelled: the bundle's stands); a Latin-script UI's is Times
        if !primary.is_empty() && !["ko", "ja", "zh"].contains(&primary.as_str()) {
            layer.standard = Some("Times New Roman".into());
        }
    }
    layer
}

/// Windows' "Korean Supplemental Fonts" (every name a family is known by).
const KOREAN_SUPPLEMENTAL: &[&str] = &[
    "Gulim", "GulimChe", "Dotum", "DotumChe", "Batang", "BatangChe", "Gungsuh", "GungsuhChe", "굴림", "굴림체", "돋움", "돋움체", "바탕",
    "바탕체", "궁서", "궁서체",
];

/// The speech voices Windows 10/11 installs with a display language (its OneCore voices, as Chrome lists them through
/// SAPI: `Microsoft <Name> - <Language> (<Region>)`), the first the default. A language without a table speaks with
/// the English (United States) ones.
pub fn windows_voices(lang: &str) -> Vec<spec::SpeechVoice> {
    let l = lang.to_ascii_lowercase();
    let primary = l.split(['-', '_']).next().unwrap_or("");
    let (names, tag, label): (&[&str], &str, &str) = match (primary, l.as_str()) {
        ("en", "en-gb") => (&["George", "Hazel", "Susan"], "en-GB", "English (United Kingdom)"),
        ("de", _) => (&["Hedda", "Katja", "Stefan"], "de-DE", "German (Germany)"),
        ("fr", "fr-ca") => (&["Caroline", "Claude", "Nathalie"], "fr-CA", "French (Canada)"),
        ("fr", _) => (&["Hortense", "Julie", "Paul"], "fr-FR", "French (France)"),
        ("es", "es-mx") => (&["Raul", "Sabina"], "es-MX", "Spanish (Mexico)"),
        ("es", _) => (&["Helena", "Laura", "Pablo"], "es-ES", "Spanish (Spain)"),
        ("it", _) => (&["Cosimo", "Elsa"], "it-IT", "Italian (Italy)"),
        ("pt", "pt-pt") => (&["Helia"], "pt-PT", "Portuguese (Portugal)"),
        ("pt", _) => (&["Daniel", "Maria"], "pt-BR", "Portuguese (Brazil)"),
        ("ja", _) => (&["Ayumi", "Haruka", "Ichiro", "Sayaka"], "ja-JP", "Japanese (Japan)"),
        ("ko", _) => (&["Heami"], "ko-KR", "Korean (Korea)"),
        ("zh", "zh-tw") => (&["Hanhan", "Yating", "Zhiwei"], "zh-TW", "Chinese (Traditional, Taiwan)"),
        ("zh", "zh-hk") => (&["Danny", "Tracy"], "zh-HK", "Chinese (Traditional, Hong Kong S.A.R.)"),
        ("zh", _) => (&["Huihui", "Kangkang", "Yaoyao"], "zh-CN", "Chinese (Simplified, PRC)"),
        ("ru", _) => (&["Irina", "Pavel"], "ru-RU", "Russian (Russia)"),
        ("pl", _) => (&["Paulina"], "pl-PL", "Polish (Poland)"),
        ("nl", _) => (&["Frank"], "nl-NL", "Dutch (Netherlands)"),
        ("tr", _) => (&["Tolga"], "tr-TR", "Turkish (Turkey)"),
        ("vi", _) => (&["An"], "vi-VN", "Vietnamese (Vietnam)"),
        ("th", _) => (&["Pattara"], "th-TH", "Thai (Thailand)"),
        _ => (&["David", "Mark", "Zira"], "en-US", "English (United States)"),
    };
    names
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let name = format!("Microsoft {n} - {label}");
            spec::SpeechVoice { voice_uri: name.clone(), name, lang: tag.into(), local_service: true, default: i == 0 }
        })
        .collect()
}

/// The WebGPU adapter of a device that states its GPU's (`webgpu`: vendor, architecture, device, description): the
/// build's captured adapter -- its features, limits and canvas format, which Chrome reports in tiers a GPU class
/// shares -- with the device's `info`, the subgroup sizes of its vendor's architecture, and any limits it states.
/// `None` for a device that states none (a Mac in the pool, a document without it): no adapter, as before.
pub fn webgpu_adapter(doc: &Document, bundle: Option<&str>) -> Option<String> {
    let w = doc.device.get("webgpu")?;
    let mut a: Value = serde_json::from_str(bundle?).ok()?;
    if a.get("null").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let text = |k: &str| w.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let (vendor, arch) = (text("vendor"), text("architecture"));
    let (lo, hi) = match (vendor.as_str(), arch.as_str()) {
        ("intel", _) => (8, 32),
        ("amd", a) if a.starts_with("gcn") => (64, 64),
        ("amd", _) => (32, 64),
        ("apple", _) => (32, 32),
        _ => (32, 32),
    };
    a["info"] = json!({
        "vendor": vendor, "architecture": arch, "device": text("device"), "description": text("description"),
        "subgroupMinSize": lo, "subgroupMaxSize": hi, "isFallbackAdapter": false,
    });
    if let Some(limits) = w.get("limits").and_then(Value::as_object) {
        for (k, v) in limits {
            a["limits"][k] = v.clone();
        }
    }
    for k in ["hpInfo", "lpInfo", "wgslFeatures"] {
        if let Some(o) = a.as_object_mut() {
            o.remove(k);
        }
    }
    Some(a.to_string())
}

/// The device's battery: a laptop's charge and whether it is on mains (`chargingTime`/`dischargingTime` unknown, as
/// Chrome on Windows reports them until the OS estimates; 0 when full and charging).
pub fn battery(doc: &Document) -> Option<spec::BatteryState> {
    let b = doc.device.get("battery")?;
    let charging = b.get("charging")?.as_bool()?;
    let level = (num(b.get("level"))? * 100.0).round() / 100.0;
    Some(spec::BatteryState {
        charging,
        level,
        charging_time: if charging && level >= 1.0 { 0.0 } else { f64::INFINITY },
        discharging_time: f64::INFINITY,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus")
    }

    fn corpus(sub: &str) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = std::fs::read_dir(corpus_dir().join("fixtures").join(sub))
            .expect("the vendored persona corpus")
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read_to_string(&p).unwrap()))
            .collect();
        v.sort();
        v
    }

    /// FORMAT.md section 5: this reader agrees with every fixture of the conformance corpus
    /// (vendored at `plugins/korvus-persona/corpus/fixtures`).
    #[test]
    fn the_shared_corpus() {
        let valid = corpus("valid");
        let invalid = corpus("invalid");
        assert!(valid.len() >= 5 && invalid.len() >= 20, "the corpus is there: {} valid, {} invalid", valid.len(), invalid.len());
        for (name, text) in &valid {
            if let Err(e) = read(text) {
                panic!("refused the valid {name}: {e:?}");
            }
        }
        for (name, text) in &invalid {
            let want = serde_json::from_str::<Value>(text).unwrap()["_expect"].as_str().unwrap().to_string();
            match read(text) {
                Ok(_) => panic!("accepted the invalid {name} (wanted {want:?})"),
                Err(e) => assert!(e.iter().any(|r| r.contains(&want)), "{name}: wanted a reason with {want:?}, got {e:?}"),
            }
        }
    }

    #[test]
    fn a_text_noise_kind_is_accepted_and_kept_while_an_unknown_one_is_refused() {
        // A real seeded persona listing "text" now loads through document::read (not just the set_persona_document
        // test bypass) -- this is what unblocks a shipped persona AND the protected-site A/B (decisions.md D94).
        let base = r#"{"persona":1,"name":"t","device":{"os":"linux","gpu":{"vendor":"Google Inc.","renderer":"ANGLE (Mesa, llvmpipe (LLVM 15), OpenGL 4.5)"},"noise":["canvas","audio","webgl","text"]}}"#;
        let doc = read(base).expect("a persona listing the text noise kind is accepted");
        assert!(
            doc.device["noise"].as_array().unwrap().iter().any(|k| k == "text"),
            "the validated document keeps the text kind"
        );
        // a genuinely unknown kind is still refused, with the message the shared corpus asserts
        let bad = base.replace("\"text\"", "\"fonts\"");
        let errs = read(&bad).expect_err("an unknown noise kind is still refused");
        assert!(errs.iter().any(|e| e.contains("noise kinds are")), "the refusal still names the noise-kind rule: {errs:?}");
    }

    #[test]
    fn a_device_s_webgl_set_is_keyed_as_the_oracle_keys_it() {
        let doc = read(r#"{"persona":1,"name":"x","device":{"os":"windows","gpu":{"vendor":"Google Inc. (Intel)","renderer":"ANGLE (Intel, X Direct3D11 vs_5_0 ps_5_0, D3D11)"},"webgl":{"webgl1":{"params":{"MAX_TEXTURE_SIZE":8192},"precision":{"VERTEX_LOW_FLOAT":[127,127,23]},"max_anisotropy":16}}}}"#).unwrap();
        let o: Value = serde_json::from_str(&device_oracle(&doc, r#"{"webgl":{"paramsByNum":{"3379":16384}}}"#).unwrap()).unwrap();
        assert_eq!(o["webgl"]["paramsByNum"]["3379"], 8192);
        assert_eq!(o["webgl"]["paramsByNum"]["34047"], 16);
        assert_eq!(o["webgl"]["unmaskedRenderer"], "ANGLE (Intel, X Direct3D11 vs_5_0 ps_5_0, D3D11)");
        assert_eq!(o["webgl"]["precision"]["VERTEX_SHADER.LOW_FLOAT"], json!([127, 127, 23]));
        assert_eq!(o["webgl"]["_document"], "x");
    }

    #[test]
    fn a_device_s_os_release_is_worn_only_where_the_build_can_be_it() {
        // Windows 11 over a Windows 11 capture, written as Chrome writes it
        assert_eq!(platform_version("windows", "15.0.0", "19.0.0").as_deref(), Some("15.0.0"));
        assert_eq!(platform_version("windows", "19.0", "19.0.0").as_deref(), Some("19.0.0"));
        // Windows 10 over a Windows 11 capture would list fonts it never had
        assert_eq!(platform_version("windows", "10.0.0", "19.0.0"), None);
        // macOS: the capture's release or a later one
        assert_eq!(platform_version("macos", "26.4.1", "14.8.9").as_deref(), Some("26.4.1"));
        assert_eq!(platform_version("macos", "12.7.6", "14.8.9"), None);
        assert_eq!(platform_version("macos", "", "14.8.9"), None);
        assert_eq!(platform_version("macos", "x.1", "14.8.9"), None);
    }

    #[test]
    fn a_worn_device_reports_its_release_voices_and_display() {
        use spec::Persona;
        let doc = read(
            r#"{"persona":1,"name":"x","device":{"os":"macos","os_version":"26.4.1","architecture":"arm","bitness":"64",
               "voices":[{"name":"Yuna","lang":"ko-KR"},{"name":"Samantha","lang":"en-US"}],
               "screen":{"width":1512,"height":982,"avail_height":949,"dpr":2,"color_gamut":"p3","hdr":true}}}"#,
        )
        .unwrap();
        let base = crate::CapturedPersona::for_document(&doc).unwrap();
        assert!(base.client_hints().platform_version.starts_with("14."), "the macOS bundle is a macOS 14 capture");
        let host = spec::coherence::HostFacts { os: "macos".into(), cores: Some(8), ..Default::default() };
        let p = wear(&doc, base, &host);
        let ch = p.client_hints();
        assert_eq!(ch.platform_version, "26.4.1");
        assert_eq!((ch.architecture.as_str(), ch.bitness.as_str()), ("arm", "64"));
        let v = p.speech_voices();
        // the device's own voices, then the build's remote (Google) ones
        assert_eq!(v.iter().take(2).map(|v| (v.name.as_str(), v.lang.as_str(), v.default)).collect::<Vec<_>>(), vec![("Yuna", "ko-KR", true), ("Samantha", "en-US", false)]);
        assert!(v[2..].iter().all(|v| !v.local_service));
        let s = Persona::screen(&p);
        assert_eq!((s.color_gamut.as_str(), s.hdr), ("p3", true));
    }

    fn windows_doc(extra: &str) -> Document {
        read(&format!(r#"{{"persona":1,"name":"w","device":{{"os":"windows","screen":{{"width":1920,"height":1080,"avail_height":1040}}{extra}}}}}"#)).unwrap()
    }

    #[test]
    fn a_device_s_layers_are_worn() {
        use spec::Persona;
        let doc = windows_doc(
            r#","window":{"outer":[1200,800],"position":[100,50]},"preferences":{"color_scheme":"dark","reduced_motion":true},
               "media_devices":["audioinput","audiooutput"],"battery":{"charging":false,"level":0.437},"storage":{"quota":306953723904},
               "audio":{"sample_rate":44100}"#,
        );
        let base = crate::CapturedPersona::for_document(&doc).unwrap();
        let host = spec::coherence::HostFacts { os: "windows".into(), cores: Some(8), ..Default::default() };
        let p = wear(&doc, base, &host);
        let s = Persona::screen(&p);
        assert_eq!((s.window_outer_width, s.window_outer_height, s.window_screen_x, s.window_screen_y), (1200, 800, 100, 50));
        assert_eq!(s.window_outer_width - s.window_inner_width, 16, "a restored Windows window's invisible borders");
        assert!(s.prefers_dark && s.reduced_motion);
        let ch = p.client_hints();
        assert_eq!(ch.media_device_kinds, ["audioinput", "audiooutput"]);
        assert_eq!(ch.storage_quota.map(|q| q.0), Some(306953723904.0));
        let b = Persona::battery(&p).unwrap();
        assert_eq!((b.charging, b.level, b.discharging_time), (false, 0.44, f64::INFINITY));
        let oracle: Value = serde_json::from_str(&p.device_oracle_json()).unwrap();
        assert_eq!(oracle["audio"]["sampleRate"], 44100.0);
    }

    #[test]
    fn a_language_decides_the_voices_the_supplemental_fonts_and_the_standard_font() {
        let doc = windows_doc("");
        let de = font_layer(&doc, "de-DE");
        assert!(de.absent.iter().any(|f| f == "Gulim") && de.standard.as_deref() == Some("Times New Roman"));
        let ko = font_layer(&doc, "ko-KR");
        assert!(ko.absent.is_empty() && ko.standard.is_none());
        assert_eq!(windows_voices("de-AT").iter().map(|v| v.name.as_str()).collect::<Vec<_>>(), ["Microsoft Hedda - German (Germany)", "Microsoft Katja - German (Germany)", "Microsoft Stefan - German (Germany)"]);
        assert!(windows_voices("de-AT")[0].default && windows_voices("xx")[0].name.contains("David"));
        let listed = windows_doc(r#","fonts":["Arial","Calibri"]"#);
        assert_eq!(font_layer(&listed, "en-US").installed, Some(vec!["Arial".to_string(), "Calibri".to_string()]));
    }

    #[test]
    fn the_new_device_fields_are_checked() {
        let bad = |extra: &str| read(&format!(r#"{{"persona":1,"name":"w","device":{{"os":"windows"{extra}}}}}"#)).is_err();
        assert!(bad(r#","media_devices":["videoinput","audioinput"]"#), "out of Chrome's order");
        assert!(bad(r#","battery":{"charging":true,"level":2}"#));
        assert!(bad(r#","preferences":{"color_scheme":"blue"}"#));
        assert!(bad(r#","audio":{"sample_rate":7}"#));
        assert!(bad(r#","window":{"position":[1,2]}"#), "a window states its frame or its size");
        assert!(!bad(r#","window":{"outer":[800,600],"position":[1,2]},"fonts":["Arial"],"storage":{"quota":1e11}"#));
    }
}
