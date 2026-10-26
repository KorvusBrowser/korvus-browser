//! R2.cal — bit-identical calibration. Render the CANONICAL probe (identical to
//! `fixtures/capture-render-fingerprint.console.js`) and compare its `getImageData` FNV hash to the real
//! browser's: the shipped persona's `render-reference` artifact (Chrome 150, raw core). A mismatch is a REAL gap
//! to close (Skia pin, text AA/hinting, emoji) — never hidden by widening a tolerance. This harness
//! measures the gap and localizes it (text rows vs circle rows) to guide calibration;
//! `r2cal_canvas_probe_is_bit_identical` is the bar (ignored until it holds).
use engine::Engine;
fn reference() -> String {
    std::fs::read_to_string(persona::shipped_artifact("render-reference")).expect("read the shipped bundle's render/reference.json")
}

/// The canonical canvas-2D probe + an in-page FNV-1a over getImageData + the 200-byte sample — byte-for-
/// byte the capture tool, so the hash + sample are directly comparable to the reference.
const PROBE: &str = r#"
const c = document.getElementById('c');
c.width = 300; c.height = 150;
const x = c.getContext('2d');
x.textBaseline = 'alphabetic';
x.fillStyle = '#f60'; x.fillRect(125, 1, 62, 20);
x.fillStyle = '#069'; x.font = '11pt Arial';
x.fillText('Korvus \u{1F600} 1.0', 2, 15);
x.fillStyle = 'rgba(102,204,0,0.7)'; x.font = '18pt Arial';
x.fillText('Korvus \u{1F600} 1.0', 4, 45);
x.globalCompositeOperation = 'multiply';
x.fillStyle = 'rgb(255,0,255)'; x.beginPath(); x.arc(50, 50, 50, 0, Math.PI * 2, true); x.closePath(); x.fill();
x.fillStyle = 'rgb(0,255,255)'; x.beginPath(); x.arc(100, 50, 50, 0, Math.PI * 2, true); x.closePath(); x.fill();
x.fillStyle = 'rgb(255,255,0)'; x.beginPath(); x.arc(75, 100, 50, 0, Math.PI * 2, true); x.closePath(); x.fill();
x.fillStyle = 'rgb(255,0,255)';
x.beginPath(); x.arc(75, 75, 75, 0, Math.PI * 2, true); x.arc(75, 75, 25, 0, Math.PI * 2, true);
x.fill('evenodd');
const d = x.getImageData(0, 0, 300, 150).data;
let h = 0x811c9dc5 >>> 0;
for (let i = 0; i < d.length; i++) { h ^= d[i]; h = (h + ((h << 1) + (h << 4) + (h << 7) + (h << 8) + (h << 24))) >>> 0; }
let ink = 0; for (let i = 3; i < d.length; i += 4) if (d[i] > 0) ink++;
const step = Math.max(1, Math.floor(d.length / 200));
let smp = []; for (let i = 0; i < d.length; i += step) smp.push(d[i]);
print('CAL:' + ('00000000' + (h >>> 0).toString(16)).slice(-8) + ':' + d.length + ':' + ink);
print('SAMPLE:' + smp.join(','));
let bin = ''; for (let i = 0; i < d.length; i++) bin += String.fromCharCode(d[i]);
print('PIXELS:' + btoa(bin));
"#;

struct Probe {
    hash: String,
    len: usize,
    ink: usize,
    sample: Vec<u8>,
    pixels: Vec<u8>,
}

async fn render_probe() -> Probe {
    use base64::Engine as _;
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_document("<!doctype html><html><body><canvas id=c></canvas></body></html>")
        .expect("load doc");
    e.eval("r2cal-probe", PROBE.to_string()).expect("probe eval");
    e.run().await.expect("run");
    let out = e.take_output();
    let cal = out.iter().find(|l| l.starts_with("CAL:")).expect("CAL: printed");
    let smp = out.iter().find(|l| l.starts_with("SAMPLE:")).expect("SAMPLE: printed");
    let pxl = out.iter().find(|l| l.starts_with("PIXELS:")).expect("PIXELS: printed");
    let parts: Vec<&str> = cal.trim_start_matches("CAL:").split(':').collect();
    let sample = smp.trim_start_matches("SAMPLE:").split(',').filter_map(|n| n.parse().ok()).collect();
    let pixels = base64::engine::general_purpose::STANDARD
        .decode(pxl.trim_start_matches("PIXELS:"))
        .expect("base64 pixels");
    Probe {
        hash: parts[0].to_string(),
        len: parts[1].parse().unwrap_or(0),
        ink: parts[2].parse().unwrap_or(0),
        sample,
        pixels,
    }
}

/// Decode the reference `toDataURL` PNG into RGBA8 (= the browser's getImageData pixels; PNG is lossless
/// straight-alpha sRGB, same as getImageData).
fn reference_pixels() -> Vec<u8> {
    let v: serde_json::Value = serde_json::from_str(&reference()).expect("reference json");
    let durl = v["canvas2d"]["dataURL"].as_str().expect("dataURL");
    let b64 = durl.strip_prefix("data:image/png;base64,").expect("png data url");
    use base64::Engine as _;
    let png_bytes = base64::engine::general_purpose::STANDARD.decode(b64).expect("base64 png");
    let decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
    let mut reader = decoder.read_info().expect("png header");
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).expect("png frame");
    buf.truncate(info.buffer_size());
    match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf.chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        other => panic!("unexpected PNG color type {other:?}"),
    }
}

fn reference_canvas() -> (String, usize, Vec<u8>) {
    let v: serde_json::Value = serde_json::from_str(&reference()).expect("reference json");
    let c = &v["canvas2d"];
    let sample = c["imageDataSample"]
        .as_array()
        .map(|a| a.iter().filter_map(|n| n.as_u64().map(|x| x as u8)).collect())
        .unwrap_or_default();
    (
        c["imageDataHash"].as_str().unwrap_or("").to_string(),
        c["imageDataLen"].as_u64().unwrap_or(0) as usize,
        sample,
    )
}

/// Diagnostic (always runs): the probe renders real content of the right size; PRINT the hash + a
/// localized sample-match report (text rows vs circle rows) so calibration progress is visible
/// (`cargo test -- --nocapture`). Asserts the pipeline, not bit-identity.
#[tokio::test]
async fn r2cal_probe_renders_and_reports_gap() {
    let k = render_probe().await;
    let (bhash, blen, bsample) = reference_canvas();
    // Sample i sits at byte i*step (step = len/200), i.e. pixel row ≈ i*0.75. Rows 0..~45 are the two
    // text strings; rows ~45..150 are the multiply circles. Bucket the sample match rate by region.
    let n = k.sample.len().min(bsample.len());
    let (mut text_ok, mut text_n, mut circ_ok, mut circ_n) = (0, 0, 0, 0);
    for i in 0..n {
        let row = (i as f32) * 0.75;
        let ok = k.sample[i] == bsample[i];
        if row < 46.0 {
            text_n += 1;
            text_ok += ok as usize;
        } else {
            circ_n += 1;
            circ_ok += ok as usize;
        }
    }
    let total_ok: usize = (0..n).filter(|&i| k.sample[i] == bsample[i]).count();
    println!(
        "R2.cal canvas: korvus={} reference={} match={} | len={} ink={} | sample {}/{} bytes | text-rows {}/{} | circle-rows {}/{}",
        k.hash, bhash, k.hash == bhash, k.len, k.ink, total_ok, n, text_ok, text_n, circ_ok, circ_n
    );
    assert_eq!(k.len, blen, "probe backing size must match the reference (300x150x4)");
    assert!(k.ink > 500, "probe must render substantial content, got {} inked", k.ink);
}

/// Full-pixel diff (always runs): decode the reference PNG and compare EVERY pixel to Korvus's, so
/// the exact residual — count, %, bounding box, and text-vs-circle split — is visible for calibration.
/// Asserts only that sizes match; the residual itself is reported, not gated (the gated bar is below).
#[tokio::test]
async fn r2cal_full_pixel_diff() {
    let k = render_probe().await;
    let reference = reference_pixels();
    assert_eq!(k.pixels.len(), reference.len(), "pixel buffer sizes differ (korvus {} reference {})", k.pixels.len(), reference.len());
    let w = 300usize;
    let (mut diff, mut text_diff, mut circ_diff) = (0usize, 0usize, 0usize);
    let (mut small, mut large) = (0usize, 0usize); // small = AA-noise (<=4), large = structural (>32)
    let mut cols = [0usize; 10]; // diff count per 30px column band (localize the emoji/glyphs)
    let mut chan_max = 0u8;
    for p in 0..(k.pixels.len() / 4) {
        let o = p * 4;
        if k.pixels[o..o + 4] != reference[o..o + 4] {
            diff += 1;
            let (x, y) = (p % w, p / w);
            if y < 46 {
                text_diff += 1;
            } else {
                circ_diff += 1;
            }
            cols[(x / 30).min(9)] += 1;
            let d = (0..4)
                .map(|c| (k.pixels[o + c] as i16 - reference[o + c] as i16).unsigned_abs() as u8)
                .max()
                .unwrap_or(0);
            chan_max = chan_max.max(d);
            if d <= 4 {
                small += 1;
            } else if d > 32 {
                large += 1;
            }
        }
    }
    let total = k.pixels.len() / 4;
    println!(
        "R2.cal full diff: {}/{} px ({:.2}%) | text {} circles {} | small(<=4/AA) {} large(>32/structural) {} maxdelta {}",
        diff, total, 100.0 * diff as f32 / total as f32, text_diff, circ_diff, small, large, chan_max
    );
    println!("R2.cal col-bands (30px each, x=0..300): {cols:?}");
    // Dump both renders side-by-side for visual inspection when R2CAL_OUT is set (wrong font / missing
    // emoji / shifted text are obvious by eye where a byte diff is not). Off by default so the gate run
    // does not litter the repo.
    if let Ok(dir) = std::env::var("R2CAL_OUT") {
        write_png(&format!("{dir}/r2cal_korvus.png"), &k.pixels, w as u32, 150);
        write_png(&format!("{dir}/r2cal_reference.png"), &reference, w as u32, 150);
        // Diff map: differing pixels bright red over a dimmed reference background — glyph-EDGE-shift halos
        // (positioning/shaping diff) look different from solid-fill diffs (rasterizer/build diff).
        let mut diffimg = vec![0u8; k.pixels.len()];
        for p in 0..(k.pixels.len() / 4) {
            let o = p * 4;
            if k.pixels[o..o + 4] != reference[o..o + 4] {
                diffimg[o..o + 4].copy_from_slice(&[255, 0, 0, 255]);
            } else {
                diffimg[o..o + 4].copy_from_slice(&[reference[o] / 4, reference[o + 1] / 4, reference[o + 2] / 4, 255]);
            }
        }
        write_png(&format!("{dir}/r2cal_diff.png"), &diffimg, w as u32, 150);
        println!("R2.cal wrote r2cal_korvus/reference/diff.png to {dir}");
    }
}

fn write_png(path: &str, rgba: &[u8], w: u32, h: u32) {
    let file = std::fs::File::create(path).expect("create png");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().expect("png header").write_image_data(rgba).expect("png data");
}

/// The bit-identical bar: Korvus's canvas probe hashes identically to the real browser on this machine.
/// Ignored until the calibration gap (Skia pin + Chrome canvas paint flags + emoji) is closed — run with
/// `cargo test -- --ignored`. Never satisfy this by widening a tolerance.
#[tokio::test]
#[ignore = "R2.cal bit-identical target; un-ignore when the probe hash matches the reference"]
async fn r2cal_canvas_probe_is_bit_identical() {
    let k = render_probe().await;
    let (bhash, _, _) = reference_canvas();
    assert_eq!(k.hash, bhash, "canvas probe not yet bit-identical to the reference");
}
