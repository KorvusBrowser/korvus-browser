//! GS.3 Gate (DoD-4, values axis): every persona-visible navigator/screen scalar the page can read
//! equals the shipped bundle's value -- and none is a "" / 0 / null stub. This is the
//! "values == profile, diff = 0" check: if a getter regresses to a default or an incoherent literal,
//! an assertion here fails.
//!
//! EVERY EXPECTATION IS READ FROM THE BUNDLE. This file used to spell out the Mac capture --
//! `MacIntel`, `hc=10`, `1512x982`, `dpr=2`, `arm/26.5.0/151.0.7922.76/macOS` -- which made it a
//! test of WHICH device shipped rather than of whether the device's values reach the page. The
//! shipped bundle became a Linux capture and every line went red for a value that had simply moved.
use engine::Engine;
use spec::Persona;

#[tokio::test]
async fn values_match_captured_profile() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.eval("v", r#"
        print("ua=" + navigator.userAgent);
        print("plat=" + navigator.platform);
        print("hc=" + navigator.hardwareConcurrency);
        print("dm=" + navigator.deviceMemory);
        print("langs=" + navigator.languages.join(",") + " lang=" + navigator.language);
        print("vendor=" + navigator.vendor + " vendorSub=[" + navigator.vendorSub + "]");
        print("appName=" + navigator.appName + " appCodeName=" + navigator.appCodeName);
        print("product=" + navigator.product + " productSub=" + navigator.productSub);
        print("appVersionOk=" + (navigator.appVersion === navigator.userAgent.replace("Mozilla/", "")));
        print("touch=" + navigator.maxTouchPoints + " cookie=" + navigator.cookieEnabled + " online=" + navigator.onLine);
        print("pdf=" + navigator.pdfViewerEnabled + " wd=" + navigator.webdriver + " dnt=" + navigator.doNotTrack);
        print("screen=" + screen.width + "x" + screen.height + " depth=" + screen.colorDepth + "/" + screen.pixelDepth);
        print("avail=" + screen.availWidth + "x" + screen.availHeight + " top=" + screen.availTop + " left=" + screen.availLeft);
        print("dpr=" + devicePixelRatio);
        // High-entropy coherence: the UA major, the brand list, and the full version must all agree.
        navigator.userAgentData.getHighEntropyValues(["architecture","platformVersion","uaFullVersion"]).then(v => {
            print("he=" + v.architecture + "/" + v.platformVersion + "/" + v.uaFullVersion + "/" + v.platform);
        });
    "#.into()).unwrap();
    e.run().await.unwrap();

    let p = persona::CapturedPersona::default();
    let nav = p.navigator();
    let scr = p.screen();
    let ch = p.client_hints();
    let x = p.navigator_extra();
    assert!(!nav.user_agent.is_empty() && !nav.platform.is_empty() && !nav.languages.is_empty(),
            "a bundle with empty identity scalars is not a persona");
    let dnt = match x.do_not_track.as_deref() { Some(v) if !v.is_empty() => v.to_string(), _ => "null".into() };
    assert_eq!(e.take_output(), vec![
        format!("ua={}", nav.user_agent),
        format!("plat={}", nav.platform),
        format!("hc={}", nav.hardware_concurrency),
        format!("dm={}", nav.device_memory),
        format!("langs={} lang={}", nav.languages.join(","), nav.languages[0]),
        format!("vendor={} vendorSub=[{}]", x.vendor, x.vendor_sub),
        format!("appName={} appCodeName={}", x.app_name, x.app_code_name),
        format!("product={} productSub={}", x.product, x.product_sub),
        "appVersionOk=true".to_string(),
        format!("touch={} cookie={} online=true", x.max_touch_points, x.cookie_enabled),
        format!("pdf={} wd=false dnt={dnt}", x.pdf_viewer_enabled),
        format!("screen={}x{} depth={}/{}", scr.width, scr.height, scr.color_depth, scr.color_depth),
        format!("avail={}x{} top={} left={}", scr.avail_width, scr.avail_height, scr.avail_top, scr.avail_left),
        format!("dpr={}", scr.device_pixel_ratio),
        format!("he={}/{}/{}/{}", ch.architecture, ch.platform_version, ch.ua_full_version, ch.platform),
    ]);
}
