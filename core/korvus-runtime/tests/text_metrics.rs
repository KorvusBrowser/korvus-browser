//! `measureText`, and the cross-check that makes it matter.
//!
//! It used to be `text.chars().count() * 7.0` -- the same number for `"iiiii"` and `"WWWWW"`.
//! `measureText(s).width` is a float a page reads to four decimal places, and an engine that
//! answers with a character count disagrees with every real font, which is a fingerprint rather
//! than an approximation.
//!
//! The numbers below came from a real Chrome (the capture's `fontMetrics` block, via `emit.py --role font-metrics`
//! captures the tables; these were measured against the same browser afterwards).
use engine::Engine;
async fn run(script: &str) -> Vec<String> {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.eval("t", script.to_string()).unwrap();
    e.run().await.unwrap();
    e.take_output()
}

// Chrome's own answers, to four decimal places.
#[tokio::test]
async fn measurements_match_a_real_browser() {
    let out = run(
        r#"
        const c = document.createElement("canvas").getContext("2d");
        for (const [f, s] of [
          ["16px Arial", "Hello, World!"],
          ["16px Arial", "AVATAR To Ya"],
          ["11pt Arial", "Korvus 1.0"],
          ["16px \"Times New Roman\"", "Hello, World!"],
          ["16px \"Courier New\"", "AVATAR To Ya"],
        ]) {
          c.font = f;
          print(c.measureText(s).width.toFixed(4));
        }
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "91.2891",   // Chrome: 91.2891 -- this engine answered 91.2890 until the advances were snapped to design units
            "103.4375",  // exact -- and 7.7px narrower than the advances add up to, because of kerning
            "70.0609",   // exact, and only because the size is floored to 1/64 px first
            "88.4688",
            "115.2188",
        ]
    );
}

// The replaced answer was the same number for both of these.
#[tokio::test]
async fn narrow_and_wide_text_of_the_same_length_measure_differently() {
    let out = run(
        r#"
        const c = document.createElement("canvas").getContext("2d");
        c.font = "16px Arial";
        const narrow = c.measureText("iiiiiiiiii").width;
        const wide = c.measureText("WWWWWWWWWW").width;
        print("narrow=" + narrow.toFixed(2) + " wide=" + wide.toFixed(2));
        print("ratio=" + (wide / narrow > 4));
        "#,
    )
    .await;
    assert_eq!(out, vec!["narrow=35.55 wide=151.02", "ratio=true"]);
}

// The metrics Chrome hands back are integers, so a fractional one is a tell on its own.
#[tokio::test]
async fn the_font_bounding_box_metrics_are_integers() {
    let out = run(
        r#"
        const c = document.createElement("canvas").getContext("2d");
        c.font = "16px Arial";
        const m = c.measureText("Hxg");
        print("asc=" + m.fontBoundingBoxAscent + " desc=" + m.fontBoundingBoxDescent);
        print("int=" + (Number.isInteger(m.fontBoundingBoxAscent)));
        "#,
    )
    .await;
    assert_eq!(out, vec!["asc=14 desc=3", "int=true"]);
}

// The other half of the shared value layer: a canvas that knew seven colour names while the cascade
// knew a hundred and forty-eight is a difference a page can read in two lines.
#[tokio::test]
async fn the_canvas_and_the_cascade_parse_the_same_colours() {
    // `r##` because the CSS below contains `"#`, which would end an `r#` string.
    let out = run(
        r##"
        const c = document.createElement("canvas").getContext("2d");
        for (const v of ["rebeccapurple", "#f008", "hsl(120 100% 50%)", "rgb(0 0 0 / 50%)", "notacolor"]) {
          c.fillStyle = "black";
          c.fillStyle = v;
          print(v + " -> " + c.fillStyle);
        }
        "##,
    )
    .await;
    // The RIGHT-HAND SIDES ARE THE BROWSER'S, recorded from Brave 151 by
    // the capture rig (see the `fillStyle` cases in
    // `core/korvus-webapi/tests/fixtures/behavior-cases/CanvasRenderingContext2D.json`). The
    // getter SERIALIZES per CSS Color -- `#rrggbb` when opaque, `rgba(r, g, b, a)` otherwise -- it
    // does not echo the string that was assigned. This test used to assert the echo, which is what
    // this engine did before the 2D surface learned to serialize.
    assert_eq!(
        out,
        vec![
            "rebeccapurple -> #663399",
            // An 8-bit hex alpha: 0x88/255, printed to three decimals as Chrome does.
            "#f008 -> rgba(255, 0, 0, 0.533)",
            "hsl(120 100% 50%) -> #00ff00",
            // 50% survives as the float CSS parsed, NOT as 128/255 = 0.502.
            "rgb(0 0 0 / 50%) -> rgba(0, 0, 0, 0.5)",
            // Unparseable: per spec the assignment is ignored and the previous style stands --
            // serialized, so the `"black"` before it reads back as `#000000`.
            "notacolor -> #000000",
        ]
    );
}

// **P3's gate.** The cross-check a detector runs: the same text, measured two ways, has to agree.
// `ctx.measureText(s).width` and the `offsetWidth` of an element containing `s` go through the same
// font tables, so they do -- to within the pixel `offsetWidth` is rounded to.
#[tokio::test]
async fn measure_text_and_offset_width_agree_on_the_same_string() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(
        r#"<!doctype html><html><head><style>
             #probe { display: inline-block; font: 16px Arial; white-space: nowrap }
           </style></head><body><span id="probe"></span></body></html>"#,
    )
    .unwrap();
    e.eval(
        "t",
        r#"
        const probe = document.getElementById("probe");
        const c = document.createElement("canvas").getContext("2d");
        c.font = "16px Arial";
        for (const s of ["Hello, World!", "AVATAR To Ya", "iiiiiiiiii", "WWWWWWWWWW",
                         "The quick brown fox", "korvus-browser"]) {
          probe.textContent = s;
          const measured = c.measureText(s).width;
          const laid = probe.offsetWidth;
          print(s + ": " + (Math.abs(measured - laid) <= 1) + " (" + measured.toFixed(2) + " vs " + laid + ")");
        }
        "#
        .into(),
    )
    .unwrap();
    e.run().await.unwrap();
    let out = e.take_output();
    for line in &out {
        assert!(line.contains(": true"), "{line}");
    }
    assert_eq!(out.len(), 6);
}

// A paragraph used to be zero pixels tall, which is as plain a tell as a zero-width body.
#[tokio::test]
async fn text_gives_a_block_a_height() {
    let out = run(
        r#"
        const p = document.createElement("p");
        p.style.font = "16px Arial";
        p.style.width = "200px";
        p.textContent = "The quick brown fox jumps over the lazy dog";
        document.body.appendChild(p);
        print("height=" + p.offsetHeight);
        const wide = document.createElement("p");
        wide.style.font = "16px Arial";
        wide.style.width = "2000px";
        wide.textContent = "The quick brown fox jumps over the lazy dog";
        document.body.appendChild(wide);
        print("wide=" + wide.offsetHeight);
        const narrow = document.createElement("p");
        narrow.style.font = "16px Arial";
        narrow.style.width = "120px";
        narrow.textContent = "The quick brown fox jumps over the lazy dog";
        document.body.appendChild(narrow);
        print("narrow=" + narrow.offsetHeight);
        "#,
    )
    .await;
    // Chrome's own answers for the same three widths: 36, 18, 54. 18px per line, because a line box
    // is taller than the font size -- Arial's ascent plus descent at 16px.
    assert_eq!(out, vec!["height=36", "wide=18", "narrow=54"]);
}

// The case every indented document is full of: whitespace between blocks must contribute nothing.
#[tokio::test]
async fn indentation_between_elements_does_not_become_a_line() {
    let out = run(
        r#"
        const d = document.createElement("div");
        d.style.width = "400px";
        d.innerHTML = "\n    <div style='height:10px'></div>\n    <div style='height:10px'></div>\n";
        document.body.appendChild(d);
        print("height=" + d.offsetHeight);
        "#,
    )
    .await;
    assert_eq!(out, vec!["height=20"], "two 10px blocks and no stray line boxes");
}
