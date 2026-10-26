//! `navigator.userAgentData` (Client Hints) and the plugin list, all sourced from the persona and
//! coherent with the UA. A fabricated brand list, an empty plugin list, or a `platform` that
//! disagrees with the user-agent string are each on their own a bot tell.
//!
//! EVERY EXPECTATION IS DERIVED FROM THE BUNDLE. It used to spell out the Mac capture -- `macOS`,
//! `arm`, `26.5.0`, `151.0.7922.76` -- so switching the shipped bundle to a Linux capture failed it
//! on a value that had simply moved. A capture's contents belong in the capture; what belongs here
//! is that the values REACH JS and AGREE with each other, which is what a page checks.
use engine::Engine;
use spec::Persona;

#[tokio::test]
async fn client_hints() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.eval("a", r#"
        const d = navigator.userAgentData;
        print("mobile=" + d.mobile);
        print("platform=" + d.platform);
        print("brands=" + d.brands.map(b => b.brand + "/" + b.version).join(","));
        d.getHighEntropyValues(["architecture", "bitness", "uaFullVersion", "platformVersion", "fullVersionList"]).then(v => {
            print("arch=" + v.architecture + ",bits=" + v.bitness + ",full=" + v.uaFullVersion + ",pv=" + v.platformVersion);
            print("fvl=" + v.fullVersionList.map(b => b.brand + "/" + b.version).join(","));
        });
        print("plugins=" + navigator.plugins.length + ",pdf=" + navigator.pdfViewerEnabled);
        print("plugin0=" + navigator.plugins[0].name + "/" + navigator.plugins[0].filename);
        print("mimes=" + navigator.mimeTypes.length + "," + navigator.mimeTypes[0].type);
        // Chrome 150 answers a NetworkInformation for navigator.connection (its capture records the member
        // and its values). Brave 151, the persona until 2026-10-16, answered `undefined` there while still
        // exposing the interface object on the global.
        print("conn=" + navigator.connection + "," + typeof globalThis.NetworkInformation);
        // Native core props must remain intact ([native code], not a JS shim).
        print("ua_native=" + Function.prototype.toString.call(
            Object.getOwnPropertyDescriptor(Object.getPrototypeOf(navigator), "userAgent").get).includes("[native code]"));
    "#.into()).unwrap();
    e.run().await.unwrap();
    let out = e.take_output();
    let p = persona::CapturedPersona::default();
    let ch = p.client_hints();
    let ua = p.navigator().user_agent;
    let brands = |bs: &[spec::Brand]| {
        bs.iter().map(|b| format!("{}/{}", b.brand, b.version)).collect::<Vec<_>>().join(",")
    };

    assert_eq!(out[0], format!("mobile={}", ch.mobile));
    assert_eq!(out[1], format!("platform={}", ch.platform));
    assert_eq!(out[2], format!("brands={}", brands(&ch.brands)));
    assert_eq!(out[3], format!("plugins={},pdf=true", ch.plugins.len()));
    let p0 = ch.plugins.first().expect("the capture records a plugin list");
    assert_eq!(out[4], format!("plugin0={}/{}", p0.name, p0.filename));
    assert!(out[5].starts_with("mimes="), "{}", out[5]);
    assert_eq!(out[6], "conn=[object NetworkInformation],function");
    assert_eq!(out[7], "ua_native=true");
    assert_eq!(
        out[8],
        format!(
            "arch={},bits={},full={},pv={}",
            ch.architecture, ch.bitness, ch.ua_full_version, ch.platform_version
        )
    );
    assert_eq!(out[9], format!("fvl={}", brands(&ch.full_version_list)));

    // The half a bundle cannot get right by construction: the hints must agree with the UA string a
    // page reads beside them. `userAgentData.platform` is one of the most-compared pairs there is.
    let want_platform = if ua.contains("Macintosh") {
        "macOS"
    } else if ua.contains("Windows") {
        "Windows"
    } else if ua.contains("Android") {
        "Android"
    } else {
        "Linux"
    };
    assert_eq!(ch.platform, want_platform, "userAgentData.platform must match the UA: {ua}");

    let ua_major = ua.split("Chrome/").nth(1).and_then(|s| s.split('.').next()).expect("a Chrome UA");
    assert!(
        ch.ua_full_version.starts_with(&format!("{ua_major}.")),
        "uaFullVersion {} must be the UA's major {ua_major}",
        ch.ua_full_version
    );
    assert!(
        ch.brands.iter().any(|b| b.version == ua_major),
        "some brand must carry the UA major {ua_major}: {}",
        brands(&ch.brands)
    );
    assert!(
        ch.full_version_list.iter().any(|b| b.version == ch.ua_full_version),
        "fullVersionList must contain uaFullVersion {}: {}",
        ch.ua_full_version,
        brands(&ch.full_version_list)
    );
    assert_eq!(ch.mobile, ua.contains("Mobile"), "the mobile bit must match the UA");
}
