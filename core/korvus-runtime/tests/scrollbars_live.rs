//! Classic scrollbars take space, and text fields are sized in the font's average and maximum
//! character widths — under EVERY captured bundle.
//!
//! Before P11b no scrollbar took space anywhere: `documentElement.clientWidth` was `innerWidth` on a
//! page taller than its window, an `overflow: scroll` box kept its whole client box, and a bare
//! `<input>` was 28px short because the persona's font table had no OS/2 average. The expected values
//! below are Google Chrome 150.0.7871.46's, read on this box (Linux, `--headless=new`, 780px window,
//! 2026-09-17) from a page served over loopback with exactly this markup — and the same page read
//! headed from Brave 151, Edge 151 and Opera 135 answered every row identically, which is also what
//! their captured `theme.json` and `fonts/metrics.json` say. The engine reads the thickness and the
//! character widths from the WORN bundle; nothing here is compiled in.
//!
//! The two `<input>` rows are the one place a Windows persona lays out differently, because Windows
//! Chrome sizes an Arial text field in a narrower average: DirectWrite reports no `fAvgCharWidth`, so
//! Blink falls back to the advance of `'x'` (narrower than the `'0'` the OS/2 average sits near), and
//! both inputs come out 177 wide where Liberation Sans on Linux gives 185. Measured on Chrome 152
//! Windows (CfT, headless, 780px window, 2026-10-17): every OTHER row — scrollbars, the Courier
//! textarea, the select — is byte-for-byte the Linux figure, so only the input width is per-OS here.
//!
//! Three rows of the brief's table are deliberately NOT asserted, each for a named reason outside
//! layout:
//!
//! ```text
//! ::-webkit-scrollbar { width: 8px }   Chrome 192x94  the cascade has no ::-webkit-scrollbar rules
//! overflow: overlay (taller)           Chrome 185x100 the cascade computes `overlay` as `visible`
//! <textarea> (no font given)           Chrome 155x44  Chrome's `monospace` is this HOST's fontconfig
//!                                                     default (Noto Sans Mono CJK), not a persona
//!                                                     font; the same textarea in 13.3333px Courier
//!                                                     New IS asserted (181x36)
//! ```
use engine::Engine;

fn bundles() -> Vec<String> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/korvus-persona/bundles");
    let mut out: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("manifest.toml").is_file())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    out.sort();
    assert!(!out.is_empty(), "no persona bundle in the tree");
    out
}

const PAGE: &str = r#"<!doctype html>
<meta charset="utf-8">
<style>
 #sc{width:200px;height:100px;overflow:scroll}
 #au{width:200px;height:100px;overflow:auto}
 #af{width:200px;height:100px;overflow:auto}
 #thin{width:200px;height:100px;overflow:scroll;scrollbar-width:thin}
 #none{width:200px;height:100px;overflow:scroll;scrollbar-width:none}
 #gut{width:200px;height:100px;overflow:hidden;scrollbar-gutter:stable}
</style>
<body>
<div id=sc></div>
<div id=au><div style="height:300px"></div></div>
<div id=af></div>
<div id=thin></div><div id=none></div><div id=gut></div>
<textarea id=tacn style='font:13.3333px "Courier New"'></textarea>
<input id=in>
<input id=in2 size=20 style="font:13.3333px Arial">
<select id=se><option>a</option></select>
<div id=fx style="position:fixed;right:0;top:0;width:10px;height:10px"></div>
<div style="height:3000px"></div>
</body>"#;

const PROBE: &str = r#"
const q = (id) => document.getElementById(id);
const cwh = (id) => q(id).clientWidth + "x" + q(id).clientHeight;
const rect = (id) => { const b = q(id).getBoundingClientRect(); return b.width + "x" + b.height; };
print("viewport scrollbar=" + (innerWidth - document.documentElement.clientWidth));
print("fixed right gap=" + (innerWidth - q("fx").getBoundingClientRect().right));
print("scroll=" + cwh("sc"));
print("auto taller=" + cwh("au"));
print("auto empty=" + cwh("af"));
print("thin=" + cwh("thin"));
print("none=" + cwh("none"));
print("gutter=" + cwh("gut"));
print("textarea courier=" + rect("tacn") + " client " + q("tacn").clientWidth);
print("input=" + rect("in"));
print("input size20=" + rect("in2"));
print("select=" + rect("se"));
"#;

/// Chrome's answers to [`PROBE`] over [`PAGE`], with the two `<input>` widths written `{box}` — 185 on
/// Linux, 177 on Windows (see the module doc). Everything else is the same figure on both.
const CHROME: &[&str] = &[
    "viewport scrollbar=15",
    "fixed right gap=15",
    "scroll=185x85",
    "auto taller=185x100",
    "auto empty=200x100",
    "thin={thin}",
    "none=200x100",
    "gutter=185x100",
    "textarea courier=181x36 client 179",
    "input={box}x21",
    "input size20={box}x21",
    "select=30x19",
];

/// The worn bundle's OS, read from its `manifest.toml` (`os = "windows"` / `"linux"`). The input width
/// is the one row that follows it.
fn bundle_os(id: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/korvus-persona/bundles")
        .join(id)
        .join("manifest.toml");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    for line in text.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("os") {
            if let Some((_, v)) = rest.split_once('=') {
                return v.trim().trim_matches('"').to_string();
            }
        }
    }
    panic!("{id}: manifest.toml has no `os` field");
}

#[tokio::test]
async fn every_bundle_lays_out_chromes_scrollbars_and_text_fields() {
    for id in bundles() {
        // A text field is `size` average characters wide, which the OS's Arial sizing sets: Windows 177
        // (DirectWrite leaves fAvgCharWidth 0 -> advance 'x'), Linux 185 (Liberation's xAvg), macOS 153
        // (measured on real macOS Chrome, 2026-10-18 -- the engine reproduces it from the mac bundle's
        // Arial metrics). The `thin` scrollbar is the worn theme's thinThickness: 10 on Linux/Windows,
        // 11 on macOS (its capture), so `thin` is 1 px narrower there.
        let (input_box, thin) = match bundle_os(&id).as_str() {
            "windows" => ("177", "190x90"),
            "macos" => ("153", "189x89"),
            _ => ("185", "190x90"),
        };
        let expect: Vec<String> =
            CHROME.iter().map(|r| r.replace("{box}", input_box).replace("{thin}", thin)).collect();
        let mut e = Engine::new(persona::worn_on_host(
            persona::CapturedPersona::select(Some(&id)).unwrap_or_else(|err| panic!("select {id}: {err}")),
        ));
        e.run().await.unwrap();
        e.load_html(PAGE).unwrap();
        e.eval("scrollbars", PROBE.to_string()).unwrap();
        e.run().await.unwrap();
        assert_eq!(e.take_output(), expect, "wearing {id}");
    }
}
