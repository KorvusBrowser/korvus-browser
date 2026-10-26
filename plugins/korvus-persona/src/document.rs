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
const DEVICE: &[&str] = &["os", "os_version", "architecture", "bitness", "model", "brand", "hardware", "screen", "gpu", "webgl", "webgpu", "voices", "window", "noise"];
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
        let ok = window.get("frame").and_then(Value::as_array).is_some_and(|f| f.len() == 2 && f.iter().all(|v| int(Some(v)).is_some_and(|n| (0..=400).contains(&n))));
        if !ok {
            out.push("window.frame is [outerWidth - innerWidth, outerHeight - innerHeight], each 0-400".into());
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
    out.window_outer_width = aw;
    out.window_outer_height = ah;
    out.window_inner_width = aw.saturating_sub(fw);
    out.window_inner_height = ah.saturating_sub(fh);
    out.window_screen_x = left;
    out.window_screen_y = top;
    if !out.orientation_type.is_empty() {
        out.orientation_type = if w >= h { "landscape-primary".into() } else { "portrait-primary".into() };
        out.orientation_angle = 0;
    }
    Some(out)
}

/// The bundle's device oracle with the device's GPU strings and WebGL set in it, when the device states them; marked
/// `_document` so the engine does not put this machine's GPU in their place (`webgl.rs` `with_device`).
pub fn device_oracle(doc: &Document, bundle_oracle: &str) -> Option<String> {
    let gpu = doc.device.get("gpu");
    let webgl = doc.device.get("webgl");
    if gpu.is_none() && webgl.is_none() {
        return None;
    }
    let mut oracle: Value = serde_json::from_str(bundle_oracle).unwrap_or_else(|_| json!({}));
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
    // the session's zone and locale (a proxy's exit place, as the identity found it), else the build's
    let session = |k: &str| doc.session.as_ref().and_then(|s| s.get(k)).and_then(Value::as_str).filter(|v| !v.is_empty());
    if let Some(tz) = session("timezone") {
        o = o.timezone(tz);
    }
    // an explicit locale (`--locale`, a context's `korvus.locale`: `$KORVUS_LOCALE`, as `apply_context` sets it) over
    // the session's: left to the session alone, `--locale en-US` moved ICU's locale (`engine::persona_env` reads the
    // variable) while `navigator.languages` and Accept-Language kept the capture's `ko-KR` -- one page, two answers
    let explicit = std::env::var("KORVUS_LOCALE").ok().map(|l| l.trim().to_string()).filter(|l| !l.is_empty());
    if let Some(tag) = explicit.as_deref().or_else(|| session("locale")) {
        o = o.locale(tag);
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE (engine extraction): `the_shared_corpus`, which validated this reader against the shared
    // persona conformance corpus (../../../shared/personas/fixtures), is omitted from the standalone
    // engine; vendor that corpus here to restore it. The format rules are still exercised below.

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
}
