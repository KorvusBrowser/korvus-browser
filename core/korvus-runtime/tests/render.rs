//! Render spine gate.
//!
//! R0 — the record→raster→readback pipeline: a recorded `fillRect` reaches the active `RenderBackend`
//! and `getImageData` reads back its pixels. Proven with the trivial `SolidBackend` (an unstyled rect
//! is opaque black), so this asserts the SEAM works end to end, not bit-identical pixels (that is R2+).
use engine::Engine;
/// Draw one `fillRect` on a 2D context, read it back with `getImageData`, and assert the rastered
/// pixels: inside the rect is opaque black, outside is transparent, and the region is the requested size.
#[tokio::test]
async fn r0_fillrect_reaches_backend_and_reads_back() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_document("<!doctype html><html><body><canvas id=c></canvas></body></html>")
        .expect("load doc");
    e.eval(
        "r0-draw",
        r#"
        const c = document.getElementById('c');
        const x = c.getContext('2d');
        x.fillRect(0, 0, 10, 10);
        const d = x.getImageData(0, 0, 20, 20);
        const inside = (5 * 20 + 5) * 4;    // (5,5) is inside the 10x10 rect
        const outside = (15 * 20 + 15) * 4; // (15,15) is outside it
        print('R0:' + d.data[inside] + ',' + d.data[inside + 1] + ',' + d.data[inside + 2] + ',' + d.data[inside + 3]
              + '|' + d.data[outside + 3] + '|' + d.width + 'x' + d.height);
        "#
        .to_string(),
    )
    .expect("eval draw");
    e.run().await.expect("run");
    let out = e.take_output();
    assert!(
        out.iter().any(|l| l == "R0:0,0,0,255|0|20x20"),
        "R0 record→raster→readback failed; output: {out:?}"
    );
}

/// R2.b.1 — a CSS `fillStyle` is parsed + recorded and colors the rasterized rect (#f60 → #ff6600);
/// the native getter reads back the SERIALIZED colour, as CSS Color specifies and every browser
/// does (`#f60` in, `#ff6600` out). It used to read back the raw assigned string, which made
/// `ctx.fillStyle = 'red'` answer `'red'` where Chrome answers `'#ff0000'`.
#[tokio::test]
async fn r2_fillstyle_colors_the_rect() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_document("<!doctype html><html><body><canvas id=c></canvas></body></html>")
        .expect("load doc");
    e.eval(
        "r2-color",
        r#"
        const x = document.getElementById('c').getContext('2d');
        x.fillStyle = '#f60';
        x.fillRect(0, 0, 10, 10);
        const p = x.getImageData(5, 5, 1, 1).data;
        print('R2:' + p[0] + ',' + p[1] + ',' + p[2] + ',' + p[3] + '|' + x.fillStyle);
        "#
        .to_string(),
    )
    .expect("eval");
    e.run().await.expect("run");
    let out = e.take_output();
    assert!(
        out.iter().any(|l| l == "R2:255,102,0,255|#ff6600"),
        "R2.b.1 fillStyle color/getter failed: {out:?}"
    );
}

/// R2.b.2 — a filled path (beginPath → arc → fill) rasters through Skia; the circle's center takes the
/// fill color, a point outside stays transparent.
#[tokio::test]
async fn r2_path_fill_draws_a_circle() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_document("<!doctype html><html><body><canvas id=c></canvas></body></html>")
        .expect("load doc");
    e.eval(
        "r2-path",
        r#"
        const x = document.getElementById('c').getContext('2d');
        x.fillStyle = '#00f';
        x.beginPath();
        x.arc(30, 30, 20, 0, Math.PI * 2, true);
        x.fill();
        const ctr = x.getImageData(30, 30, 1, 1).data; // circle center
        const out = x.getImageData(70, 70, 1, 1).data; // outside the circle
        print('RP:' + ctr[0] + ',' + ctr[1] + ',' + ctr[2] + ',' + ctr[3] + '|' + out[3]);
        "#
        .to_string(),
    )
    .expect("eval");
    e.run().await.expect("run");
    let out = e.take_output();
    assert!(
        out.iter().any(|l| l == "RP:0,0,255,255|0"),
        "R2.b.2 path fill (circle) failed: {out:?}"
    );
}

/// R2.b.3 — fillText renders glyphs through Skia + the OS font stack: drawing 'A' inks pixels where a
/// blank canvas had none. (Bit-identical pixel calibration vs the Brave reference is R2.cal.)
#[tokio::test]
async fn r2_fill_text_renders_glyphs() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_document("<!doctype html><html><body><canvas id=c></canvas></body></html>")
        .expect("load doc");
    e.eval(
        "r2-text",
        r#"
        const x = document.getElementById('c').getContext('2d');
        x.fillStyle = '#000';
        x.font = '30px sans-serif';
        x.fillText('A', 10, 40);
        const d = x.getImageData(0, 0, 60, 50).data;
        let ink = 0;
        for (let i = 3; i < d.length; i += 4) if (d[i] > 0) ink++;
        print('RT:' + (ink > 0 ? 'inked' : 'blank') + ':' + ink + '|' + x.font);
        "#
        .to_string(),
    )
    .expect("eval");
    e.run().await.expect("run");
    let out = e.take_output();
    assert!(
        out.iter().any(|l| l.starts_with("RT:inked:") && l.ends_with("|30px sans-serif")),
        "R2.b.3 text render/font-getter failed: {out:?}"
    );
}

/// R2.b.4 — globalCompositeOperation blends subsequent draws: red, then multiply-cyan over it, yields a
/// black overlap (multiply(255,0,0)·(0,255,255) = 0,0,0). The getter reads back the mode.
#[tokio::test]
async fn r2_composite_multiply() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_document("<!doctype html><html><body><canvas id=c></canvas></body></html>")
        .expect("load doc");
    e.eval(
        "r2-comp",
        r#"
        const x = document.getElementById('c').getContext('2d');
        x.fillStyle = '#f00'; x.fillRect(0, 0, 20, 20);
        x.globalCompositeOperation = 'multiply';
        x.fillStyle = '#0ff'; x.fillRect(0, 0, 20, 20);
        const p = x.getImageData(5, 5, 1, 1).data;
        print('RC:' + p[0] + ',' + p[1] + ',' + p[2] + ',' + p[3] + '|' + x.globalCompositeOperation);
        "#
        .to_string(),
    )
    .expect("eval");
    e.run().await.expect("run");
    let out = e.take_output();
    assert!(
        out.iter().any(|l| l == "RC:0,0,0,255|multiply"),
        "R2.b.4 composite multiply failed: {out:?}"
    );
}
