//! A stylesheet's `url()` is the stylesheet's through `var()`, and a font family is
//! more than one file, in a live engine over a loopback server whose access log is the truth.
//!
//! # The oracle
//!
//! Stock Chrome 150.0.7871.46, headless, attached over CDP, on the same markup served from a
//! loopback server that logs every path. Two defects, measured as fixtures before this file existed:
//!
//! ```text
//! E20  a url() carried through a CSS CUSTOM PROPERTY resolved against the DOCUMENT
//!        .v  { --u: url(varimg.png); background-image: var(--u) }   in /sub/v.css
//!          chrome  /sub/varimg.png            korvus before  /varimg.png
//!        .vf { background-image: var(--nope, url(varfallback.png)) } in /sub/v.css
//!          chrome  /sub/varfallback.png       korvus before  /varfallback.png
//!        the direct `url()` in the same sheet was already right, which puts the defect at
//!        SUBSTITUTION time and not at parse time.
//! E21  of several @font-face rules for one family differing only in unicode-range, only the FIRST
//!      matching face was fetched
//!        four faces, U+0000-00FF / U+0100-024F / U+0400-045F / U+4E00-9FFF, text "Latin ABC Ā Ё"
//!          chrome  f_latin + f_latinext + f_cyrillic (never f_cjk)   korvus before  f_latin alone
//! ```
//!
//! # Which base, measured rather than assumed
//!
//! The brief's rule was "the sheet that DECLARED the custom property". Chrome's rule is the sheet
//! holding the declaration that USES it, and the two disagree whenever a property crosses a sheet
//! boundary. Every row below is stock Chrome 150 on this fixture:
//!
//! ```text
//! --shared declared in /sub/v.css, used by /other/cross.css   /other/sharedimg.png   (NOT /sub/)
//! --docshared declared in a <style>, used by /sub/v.css       /sub/docshared.png     (NOT /)
//! --shared declared in /sub/v.css, used by a style attribute  /sharedimg.png         (the document)
//! --iv declared and used inside an @import-ed /sub/i.css      /sub/impimg.png
//! getComputedStyle(el).getPropertyValue("--u")                url(varimg.png)  -- still RELATIVE
//! ```
//!
//! The last row is why this cannot be done when the declaration is parsed: a custom property's value
//! is raw tokens, and Chrome hands them back exactly as written.
//!
//! # Which faces, measured rather than assumed
//!
//! Chrome's `@font-face` selection runs PER CHARACTER of the rendered text, and each answer below
//! was taken with VALID woff2 bytes behind every URL -- with a body Chrome cannot decode it retries
//! the next face in the family and the request set gains faces it would never otherwise ask for,
//! which made `f_ova` and `f_ov1a` look wanted on the first pass and they are not.
//!
//! ```text
//! two faces of one family whose ranges OVERLAP on the character   the LAST declared face
//! two indistinguishable faces of one family                       the LAST declared face
//! `font-family: Chain1, Chain2`, Chain1 latin-only, text "A Ё"    f_c1 AND f_c2
//! `font-family: serif, After`, text "Ё"                           nothing -- the generic ends it
//! a ::before box's own content                                    every face its characters need
//! an <input value="V Ё">                                          both faces
//! 400 and 700 faces in each of two ranges, normal text            the two 400 faces
//! an element holding only `&nbsp;`                                fetched  (U+00A0 does not collapse)
//! an element holding only ASCII spaces                            NOT fetched
//! ```

use engine::Engine;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

type Log = Arc<Mutex<Vec<String>>>;

/// `--u`, `--shared`, `--docshared` and `--iv` each reach a `background-image` through `var()`, from
/// four different sheets, and `#inline` reaches one from a `style` attribute.
const VARS: &str = r#"<!doctype html>
<html><head>
<link rel=stylesheet href="/sub/v.css">
<link rel=stylesheet href="/other/cross.css">
<style>
.docvar { --du: url(docimg.png); background-image: var(--du) }
:root { --docshared: url(docshared.png) }
</style>
</head><body>
<div class=d>d</div>
<div class=v>v</div>
<div class=vf>vf</div>
<div class=docvar>docvar</div>
<div class=cross>cross</div>
<div class=imp>imp</div>
<div class=usedoc>usedoc</div>
<div id=inline style="--iu: url(inlineimg.png); background-image: var(--iu)">inline</div>
<div id=inlinecross style="background-image: var(--shared)">inlinecross</div>
</body></html>"#;

const V_CSS: &str = r#"@import url(i.css);
.d  { background: url(dirimg.png) }
.v  { --u: url(varimg.png); background-image: var(--u) }
.vf { background-image: var(--nope, url(varfallback.png)) }
:root { --shared: url(sharedimg.png) }
.usedoc { background-image: var(--docshared) }"#;

const I_CSS: &str = ".imp { --iv: url(impimg.png); background-image: var(--iv) }";
const CROSS_CSS: &str = ".cross { background-image: var(--shared) }";

/// The brief's E21 fixture: four faces of one family, disjoint `unicode-range`s, and text that needs
/// three of them.
const FONTS: &str = r#"<!doctype html>
<html><head>
<style>
@font-face { font-family: Multi; src: url(f_latin.woff2) format("woff2"); unicode-range: U+0000-00FF }
@font-face { font-family: Multi; src: url(f_latinext.woff2) format("woff2"); unicode-range: U+0100-024F }
@font-face { font-family: Multi; src: url(f_cyrillic.woff2) format("woff2"); unicode-range: U+0400-045F }
@font-face { font-family: Multi; src: url(f_cjk.woff2) format("woff2"); unicode-range: U+4E00-9FFF }
.m { font-family: Multi }
</style>
</head><body>
<div class=m>Latin ABC &#x100; &#x401;</div>
</body></html>"#;

/// The shapes the brief asks Chrome about: overlapping ranges, a family-list fallback chain, a face
/// reached only by a `::before`, `font-display`, weights inside ranges, and white space.
const FONTS2: &str = r#"<!doctype html>
<html><head>
<style>
@font-face { font-family: Ov; src: url(f_ova.woff2) format("woff2"); unicode-range: U+0041-005A }
@font-face { font-family: Ov; src: url(f_ovb.woff2) format("woff2"); unicode-range: U+0041-007A }
.ov { font-family: Ov }
@font-face { font-family: Tie; src: url(f_tie_a.woff2) format("woff2") }
@font-face { font-family: Tie; src: url(f_tie_b.woff2) format("woff2") }
.tie { font-family: Tie }
@font-face { font-family: Chain1; src: url(f_c1.woff2) format("woff2"); unicode-range: U+0041-005A }
@font-face { font-family: Chain2; src: url(f_c2.woff2) format("woff2"); unicode-range: U+0400-045F }
.chain { font-family: Chain1, Chain2 }
@font-face { font-family: After; src: url(f_after.woff2) format("woff2"); unicode-range: U+0400-045F }
.afterserif { font-family: serif, After }
@font-face { font-family: Pseu; src: url(f_p_latin.woff2) format("woff2"); unicode-range: U+0000-00FF }
@font-face { font-family: Pseu; src: url(f_p_cyr.woff2) format("woff2"); unicode-range: U+0400-045F }
.pb::before { content: "B \401"; font-family: Pseu }
@font-face { font-family: Disp; src: url(f_d_latin.woff2) format("woff2"); unicode-range: U+0000-00FF; font-display: swap }
@font-face { font-family: Disp; src: url(f_d_cyr.woff2) format("woff2"); unicode-range: U+0400-045F; font-display: swap }
.disp { font-family: Disp }
@font-face { font-family: Ctl; src: url(f_ctl_latin.woff2) format("woff2"); unicode-range: U+0000-00FF }
@font-face { font-family: Ctl; src: url(f_ctl_cyr.woff2) format("woff2"); unicode-range: U+0400-045F }
.ctl { font-family: Ctl }
@font-face { font-family: Wt; src: url(f_wt400_latin.woff2) format("woff2"); unicode-range: U+0000-00FF; font-weight: 400 }
@font-face { font-family: Wt; src: url(f_wt700_latin.woff2) format("woff2"); unicode-range: U+0000-00FF; font-weight: 700 }
@font-face { font-family: Wt; src: url(f_wt400_cyr.woff2) format("woff2"); unicode-range: U+0400-045F; font-weight: 400 }
@font-face { font-family: Wt; src: url(f_wt700_cyr.woff2) format("woff2"); unicode-range: U+0400-045F; font-weight: 700 }
.wt { font-family: Wt }
@font-face { font-family: Nbsp; src: url(f_nbsp.woff2) format("woff2"); unicode-range: U+0000-00FF }
.nbsp { font-family: Nbsp }
@font-face { font-family: Sp; src: url(f_sp.woff2) format("woff2"); unicode-range: U+0000-00FF }
.sp { font-family: Sp }
</style>
</head><body>
<div class=ov>Az</div>
<div class=tie>T</div>
<div class=chain>A &#x401;</div>
<div class=afterserif>&#x401;</div>
<div class=pb></div>
<div class=disp>D &#x401;</div>
<input class=ctl value="V &#x401;">
<div class=wt>W &#x401;</div>
<div class=nbsp>&nbsp;</div>
<div class=sp>   </div>
</body></html>"#;

/// Text replaced by script after `load`, on a page whose `setInterval` keeps the event loop alive:
/// the engine's rendering-opportunity frame is UNREF'D, so on a page that drains the moment the
/// mutation lands there is no next frame to notice it. Chrome, which never drains, fetches the new
/// face either way.
const LATE: &str = r#"<!doctype html>
<html><head>
<style>
@font-face { font-family: Late; src: url(g_l_latin.woff2) format("woff2"); unicode-range: U+0000-00FF }
@font-face { font-family: Late; src: url(g_l_cyr.woff2) format("woff2"); unicode-range: U+0400-045F }
.late { font-family: Late }
</style>
</head><body>
<div class=late>L</div>
<script>
setInterval(() => {}, 40);
addEventListener('load', () => {
  setTimeout(() => { document.querySelector('.late').textContent = 'L Ё'; }, 50);
});
</script>
</body></html>"#;

async fn serve(listener: TcpListener, log: Log) {
    loop {
        let Ok((mut s, _)) = listener.accept().await else { return };
        let log = log.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            let Ok(n) = s.read(&mut buf).await else { return };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            if let Ok(mut l) = log.lock() {
                l.push(path.clone());
            }
            let (ctype, body) = match path.as_str() {
                "/sub/v.css" => ("text/css", V_CSS),
                "/sub/i.css" => ("text/css", I_CSS),
                "/other/cross.css" => ("text/css", CROSS_CSS),
                p if p.ends_with(".png") => ("image/png", "x"),
                p if p.ends_with(".woff2") => ("font/woff2", "x"),
                _ => ("text/plain", "no"),
            };
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: {ctype}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(body.as_bytes()).await;
            let _ = s.flush().await;
        });
    }
}

/// Load `doc` at `path` and pump for `settle` past the load event, so the unref'd style frame gets
/// its turn on a page that has something left to run.
async fn load(doc: &str, path: &str, settle: Duration) -> (Engine, Log, String) {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener, log.clone()));
    let origin = format!("http://{addr}");
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_page(doc, &format!("{origin}{path}"), &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(8)).await;
    if !settle.is_zero() {
        let _ = e.pump(settle).await;
    }
    (e, log, origin)
}

async fn ask(e: &mut Engine, expr: &str) -> String {
    e.eval("p15a-ask", format!("print(String({expr}))")).expect("eval");
    e.run().await.expect("run");
    e.take_output().into_iter().next().unwrap_or_default()
}

fn paths(log: &Log) -> Vec<String> {
    let mut got: Vec<String> = log.lock().unwrap().clone();
    got.sort();
    got.dedup();
    got
}

#[tokio::test]
async fn a_url_through_a_custom_property_resolves_against_the_sheet_that_uses_it() {
    let (mut e, log, origin) = load(VARS, "/vars.html", Duration::ZERO).await;
    let bg = |sel: &str| format!("getComputedStyle(document.querySelector('{sel}')).backgroundImage");

    // A direct `url()` was already right; it is here so a fix cannot be "absolutize everything
    // against the document" wearing a different hat.
    assert_eq!(ask(&mut e, &bg(".d")).await, format!("url(\"{origin}/sub/dirimg.png\")"));
    // E20 itself: through a custom property, and through a reference's own fallback.
    assert_eq!(ask(&mut e, &bg(".v")).await, format!("url(\"{origin}/sub/varimg.png\")"));
    assert_eq!(ask(&mut e, &bg(".vf")).await, format!("url(\"{origin}/sub/varfallback.png\")"));
    // A `<style>` has no URL of its own, so its values resolve against the document.
    assert_eq!(ask(&mut e, &bg(".docvar")).await, format!("url(\"{origin}/docimg.png\")"));
    // Across a sheet boundary, BOTH WAYS. The user's sheet decides, not the declarer's.
    assert_eq!(ask(&mut e, &bg(".cross")).await, format!("url(\"{origin}/other/sharedimg.png\")"));
    assert_eq!(ask(&mut e, &bg(".usedoc")).await, format!("url(\"{origin}/sub/docshared.png\")"));
    // An `@import`-ed sheet is its own base.
    assert_eq!(ask(&mut e, &bg(".imp")).await, format!("url(\"{origin}/sub/impimg.png\")"));
    // A `style` attribute resolves against the document, whichever sheet the property came from.
    assert_eq!(ask(&mut e, &bg("#inline")).await, format!("url(\"{origin}/inlineimg.png\")"));
    assert_eq!(ask(&mut e, &bg("#inlinecross")).await, format!("url(\"{origin}/sharedimg.png\")"));
    // The custom property's OWN value is raw tokens and stays relative, exactly as Chrome reports
    // it -- which is why the resolution cannot happen when the declaration is parsed.
    assert_eq!(
        ask(&mut e, "getComputedStyle(document.querySelector('.v')).getPropertyValue('--u')").await,
        "url(varimg.png)"
    );

    // `load_page` is handed the document, so the page itself is not in this log; `/favicon.ico` is.
    assert_eq!(
        paths(&log),
        vec![
            "/docimg.png",
            "/favicon.ico",
            "/inlineimg.png",
            "/other/cross.css",
            "/other/sharedimg.png",
            "/sharedimg.png",
            "/sub/dirimg.png",
            "/sub/docshared.png",
            "/sub/i.css",
            "/sub/impimg.png",
            "/sub/v.css",
            "/sub/varfallback.png",
            "/sub/varimg.png",
        ],
        "the access log -- `/docshared.png` is NOT in it: the only user of `--docshared` is in \
         /sub/v.css, so the document-relative spelling is never asked for"
    );
}

#[tokio::test]
async fn every_font_face_whose_range_covers_the_text_is_fetched_and_no_other() {
    let (_e, log, _origin) = load(FONTS, "/fonts.html", Duration::ZERO).await;
    assert_eq!(
        paths(&log),
        vec!["/f_cyrillic.woff2", "/f_latin.woff2", "/f_latinext.woff2", "/favicon.ico"],
        "three of four faces -- and never /f_cjk.woff2, which no character needs"
    );
}

#[tokio::test]
async fn face_selection_runs_per_character_through_the_whole_family_list() {
    let (_e, log, _origin) = load(FONTS2, "/fonts2.html", Duration::ZERO).await;
    assert_eq!(
        paths(&log),
        vec![
            "/f_c1.woff2",          // the chain's first family answers for 'A'
            "/f_c2.woff2",          // and its SECOND for 'Ё' -- the walk does not stop at Chain1
            "/f_ctl_cyr.woff2",     // an <input value> is text the element renders
            "/f_ctl_latin.woff2",
            "/f_d_cyr.woff2",       // font-display changes nothing about what is needed
            "/f_d_latin.woff2",
            "/f_nbsp.woff2",        // U+00A0 does not collapse, so it renders and needs a face
            "/f_ovb.woff2",         // overlapping ranges: the LAST declared face wins
            "/f_p_cyr.woff2",       // a ::before box's own content
            "/f_p_latin.woff2",
            "/f_tie_b.woff2",       // two indistinguishable faces: the LAST declared one
            "/f_wt400_cyr.woff2",   // the weight filter still narrows, inside each range
            "/f_wt400_latin.woff2",
            "/favicon.ico",
        ],
        "no /f_after.woff2 (a generic ends the search), no /f_ova, /f_tie_a, /f_wt700_*, /f_sp"
    );
}

#[tokio::test]
async fn text_added_by_script_after_load_fetches_the_face_its_characters_need() {
    let (_e, log, _origin) = load(LATE, "/late.html", Duration::from_millis(600)).await;
    assert_eq!(
        paths(&log),
        vec!["/favicon.ico", "/g_l_cyr.woff2", "/g_l_latin.woff2"],
        "the cyrillic face arrives with the text the script added"
    );
}

/// Generated content that shows the `style` attribute's own text: an `opacity`-only rewrite moves no box, but it
/// changes the characters `::before` renders, and so the `unicode-range` face they need. Chrome 152.0.7977.82
/// (2026-10-23, real-time headless over CDP) requests `U+36`'s face once `opacity: 0.5` becomes `opacity: 0.6`;
/// this engine's scan, keyed on the geometry, never ran again and never asked.
const ATTR_STYLE: &str = r#"<!doctype html>
<html><head>
<style>
@font-face { font-family: AttrStyle; src: url(g_as_six.woff2) format("woff2"); unicode-range: U+36 }
#e::before { content: attr(style); font-family: AttrStyle }
</style>
</head><body>
<div id=e style="opacity: 0.5;">x</div>
<script>
setInterval(() => {}, 40);
addEventListener('load', () => {
  setTimeout(() => { document.getElementById('e').style.opacity = '0.6'; }, 50);
});
</script>
</body></html>"#;

#[tokio::test]
async fn an_opacity_rewrite_shown_by_attr_style_fetches_the_face_its_new_text_needs() {
    let (_e, log, _origin) = load(ATTR_STYLE, "/attrstyle.html", Duration::from_millis(600)).await;
    assert_eq!(paths(&log), vec!["/favicon.ico", "/g_as_six.woff2"], "the '6' of `opacity: 0.6;` needs the face");
}
