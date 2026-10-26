//! `document.featurePolicy` on the LIVE surface, against Chrome's own answers.
//!
//! Every expectation here was read off a real browser on 2026-09-18, not derived from the spec: a
//! loopback server served the fixture, set the header the row names, and the page posted its answers
//! back to the same server. Stock `/usr/bin/google-chrome` 150.0.7871.46 is the oracle for the
//! numbers; `/usr/bin/brave-browser` 151.1.93.138, `/usr/bin/microsoft-edge` 151.0.4129.93 and
//! `/usr/bin/opera` 135.0.5973.133 were asked the same questions and agreed on every shape assertion
//! below — they differ only in WHICH features they ship, which
//! `core/korvus-webapi/src/core/feature_policy/mod.rs`'s header records as an open ruling. Chrome 152.0.7977.82
//! renamed the interface `PermissionsPolicy` (`FeaturePolicy` is the same object under a legacy name): the tag,
//! the prototype's `Symbol.toStringTag` and every message name it, re-measured 2026-10-16.
//!
//! The defect this closes: `document.featurePolicy` was `null`, so `gtag`'s
//! `featurePolicy.allowedFeatures().includes("attribution-reporting")` threw and every GA4 hit lost
//! its `are=1` — 28 of the 504 replay rows. (Chrome 152 has no `attribution-reporting` feature at all, so that
//! test is false there too; `browsing-topics`, another feature whose default is `*`, takes its place below.)

use engine::Engine;
use std::time::Duration;

const ORIGIN: &str = "http://127.0.0.1:8123";
const URL: &str = "http://127.0.0.1:8123/page.html";

/// Load a document DELIVERED under `headers` and evaluate `js`, as JSON.
async fn delivered(headers: &[(&str, &str)], js: &str) -> serde_json::Value {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    let doc = "<!doctype html><html><head><title>t</title></head><body>x</body></html>";
    let hdrs: Vec<(String, String)> = std::iter::once(("content-type".to_string(), "text/html; charset=utf-8".to_string()))
        .chain(headers.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())))
        .collect();
    e.load_page_delivered(doc, URL, &[], &hdrs).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    e.eval_value("fp", js.to_string()).expect("eval")
}

async fn plain(js: &str) -> serde_json::Value {
    delivered(&[], js).await
}

fn strings(v: &serde_json::Value) -> Vec<String> {
    v.as_array().expect("an array").iter().map(|x| x.as_str().expect("a string").to_string()).collect()
}

/// The interface object, exactly as all four browsers record it.
#[tokio::test]
async fn the_interface_object_is_chromes() {
    let v = plain(
        r#"({
            tag: Object.prototype.toString.call(document.featurePolicy),
            same: document.featurePolicy === document.featurePolicy,
            protoMembers: Object.getOwnPropertyNames(FeaturePolicy.prototype),
            tagOnProto: FeaturePolicy.prototype[Symbol.toStringTag],
            ctorLength: FeaturePolicy.length,
            native: Object.getOwnPropertyNames(FeaturePolicy.prototype)
                .filter(n => typeof FeaturePolicy.prototype[n] === "function")
                .every(n => String(FeaturePolicy.prototype[n]).includes("[native code]")),
            lengths: ["allowedFeatures", "allowsFeature", "features", "getAllowlistForFeature"]
                .map(n => FeaturePolicy.prototype[n].length),
            docDesc: (d => [!!d.get, !!d.set, d.enumerable, d.configurable])(
                Object.getOwnPropertyDescriptor(Document.prototype, "featurePolicy")),
            permissionsPolicy: String(document.permissionsPolicy),
        })"#,
    )
    .await;
    assert_eq!(v["tag"], "[object PermissionsPolicy]");
    assert_eq!(v["same"], true, "document.featurePolicy must be the same object every read");
    // The ORDER is Chrome's: `constructor` last, which the generated registry's row had first.
    assert_eq!(
        strings(&v["protoMembers"]),
        ["allowedFeatures", "allowsFeature", "features", "getAllowlistForFeature", "constructor"]
    );
    assert_eq!(v["tagOnProto"], "PermissionsPolicy");
    assert_eq!(v["ctorLength"], 0);
    assert_eq!(v["native"], true);
    assert_eq!(v["lengths"], serde_json::json!([0, 1, 0, 1]));
    assert_eq!(v["docDesc"], serde_json::json!([true, false, true, true]));
    // Measured `undefined` in all four browsers: `permissionsPolicy` is NOT a member.
    assert_eq!(v["permissionsPolicy"], "undefined");
}

/// The constructor is illegal and every method rejects a foreign receiver BEFORE it counts arguments.
#[tokio::test]
async fn the_constructor_and_the_receiver_check_throw_chromes_messages() {
    let v = plain(
        r#"(() => {
            const t = f => { try { f(); return "no-throw"; } catch (e) { return e.name + ": " + e.message; } };
            return {
                newProbe: t(() => new FeaturePolicy()),
                callProbe: t(() => FeaturePolicy()),
                onProto: t(() => FeaturePolicy.prototype.allowedFeatures()),
                onForeign: t(() => FeaturePolicy.prototype.getAllowlistForFeature.call({}, "camera")),
                // The receiver check is FIRST: a foreign receiver with no arguments is still
                // "Illegal invocation", not "1 argument required".
                onForeignNoArgs: t(() => FeaturePolicy.prototype.allowsFeature.call({})),
                arity: t(() => document.featurePolicy.allowsFeature()),
                arity2: t(() => document.featurePolicy.getAllowlistForFeature()),
            };
        })()"#,
    )
    .await;
    assert_eq!(v["newProbe"], "TypeError: Failed to construct 'PermissionsPolicy': Illegal constructor");
    assert_eq!(v["callProbe"], "TypeError: Illegal constructor");
    assert_eq!(v["onProto"], "TypeError: Illegal invocation");
    assert_eq!(v["onForeign"], "TypeError: Illegal invocation");
    assert_eq!(v["onForeignNoArgs"], "TypeError: Illegal invocation");
    assert_eq!(
        v["arity"],
        "TypeError: Failed to execute 'allowsFeature' on 'PermissionsPolicy': 1 argument required, but only 0 present."
    );
    assert_eq!(
        v["arity2"],
        "TypeError: Failed to execute 'getAllowlistForFeature' on 'PermissionsPolicy': 1 argument required, but only 0 present."
    );
}

/// A document delivered with NO policy header: every feature allowed, and the defaults are the ones
/// Chrome hands back — `["*"]` or `[the document's own origin]`, per feature.
#[tokio::test]
async fn a_document_with_no_header_allows_every_feature() {
    let v = plain(
        r#"(() => {
            const p = document.featurePolicy;
            return {
                features: p.features(),
                allowed: p.allowedFeatures(),
                ar: p.getAllowlistForFeature("browsing-topics"),
                camera: p.getAllowlistForFeature("camera"),
                usb: p.getAllowlistForFeature("usb"),
                bogus: p.getAllowlistForFeature("korvus-not-a-feature"),
                allowsCamera: p.allowsFeature("camera"),
                allowsCameraElsewhere: p.allowsFeature("camera", "https://example.com"),
                allowsArElsewhere: p.allowsFeature("browsing-topics", "https://example.com"),
                allowsBogus: p.allowsFeature("korvus-not-a-feature"),
            };
        })()"#,
    )
    .await;
    let features = strings(&v["features"]);
    // The count and the first five names, verbatim from Chrome 152 on this box.
    assert_eq!(features.len(), 80);
    assert_eq!(
        &features[..5],
        ["geolocation", "ch-ua-full-version-list", "cross-origin-isolated", "screen-wake-lock", "on-device-speech-recognition"]
    );
    // THE ROW THE 28 REPLAY ROWS RODE ON: `gtag`'s test -- false in Chrome 152, which dropped the feature.
    assert!(!features.contains(&"attribution-reporting".to_string()));
    assert_eq!(strings(&v["allowed"]), features, "with no header, allowedFeatures() IS features()");
    assert_eq!(strings(&v["ar"]), ["*"], "browsing-topics defaults to *");
    assert_eq!(strings(&v["camera"]), [ORIGIN], "camera defaults to self");
    assert_eq!(strings(&v["usb"]), [ORIGIN]);
    assert!(strings(&v["bogus"]).is_empty(), "an unknown feature is the empty list, not a throw");
    assert_eq!(v["allowsCamera"], true);
    assert_eq!(v["allowsCameraElsewhere"], false, "a `self` default admits only this origin");
    assert_eq!(v["allowsArElsewhere"], true, "a `*` default admits every origin");
    assert_eq!(v["allowsBogus"], false);
}

/// The header grammar, row for row against the table in `core/feature_policy/mod.rs`'s header.
#[tokio::test]
async fn the_permissions_policy_header_is_parsed_the_way_chrome_parses_it() {
    let probe = r#"(() => {
        const p = document.featurePolicy;
        return {
            geo: p.getAllowlistForFeature("geolocation"),
            camera: p.getAllowlistForFeature("camera"),
            ar: p.getAllowlistForFeature("browsing-topics"),
            usb: p.getAllowlistForFeature("usb"),
            nAllowed: p.allowedFeatures().length,
            hasAr: p.allowedFeatures().includes("browsing-topics"),
        };
    })()"#;

    // The whole policy of the L22 fixture, in one header.
    let v = delivered(
        &[("permissions-policy", r#"geolocation=(), camera=(self "https://example.com"), browsing-topics=(), usb=*"#)],
        probe,
    )
    .await;
    assert!(strings(&v["geo"]).is_empty());
    assert_eq!(strings(&v["camera"]), [ORIGIN, "https://example.com"]);
    assert!(strings(&v["ar"]).is_empty());
    assert_eq!(strings(&v["usb"]), ["*"]);
    assert_eq!(v["nAllowed"], 78, "two of the 80 are removed: geolocation and browsing-topics");
    assert_eq!(v["hasAr"], false, "browsing-topics is off under this header");

    // A member with no `=` is `()`.
    let v = delivered(&[("permissions-policy", "geolocation")], probe).await;
    assert!(strings(&v["geo"]).is_empty());

    // The LAST declaration of a feature wins, inside one header and across two.
    let v = delivered(&[("permissions-policy", "geolocation=(), geolocation=*")], probe).await;
    assert_eq!(strings(&v["geo"]), ["*"]);

    // Feature names are case-SENSITIVE: an upper-case one declares nothing.
    let v = delivered(&[("permissions-policy", "GEOLOCATION=()")], probe).await;
    assert_eq!(strings(&v["geo"]), [ORIGIN]);

    // An unknown feature is skipped and the rest of the dictionary still applies.
    let v = delivered(&[("permissions-policy", "korvus-nope=(), geolocation=()")], probe).await;
    assert!(strings(&v["geo"]).is_empty());

    // Garbage and an empty header declare nothing.
    for header in ["this is not a policy", ""] {
        let v = delivered(&[("permissions-policy", header)], probe).await;
        assert_eq!(strings(&v["geo"]), [ORIGIN], "header {header:?}");
    }

    // `*` anywhere in an inner list wins; an unrecognised token drops out of it.
    let v = delivered(&[("permissions-policy", "camera=(self *)")], probe).await;
    assert_eq!(strings(&v["camera"]), ["*"]);
    let v = delivered(&[("permissions-policy", "camera=(none)")], probe).await;
    assert!(strings(&v["camera"]).is_empty());

    // An origin token is ORIGIN-serialised: the trailing slash is gone.
    let v = delivered(&[("permissions-policy", r#"camera=("https://example.com/")"#)], probe).await;
    assert_eq!(strings(&v["camera"]), ["https://example.com"]);

    // The legacy `Feature-Policy` header still counts (measured: Chrome 150 honours it).
    let v = delivered(&[("feature-policy", "geolocation 'none'")], probe).await;
    assert!(strings(&v["geo"]).is_empty());
}

/// `iframe.featurePolicy` — the CONTAINER policy: the document's, with the element's `allow` over it.
#[tokio::test]
async fn an_iframes_container_policy_applies_its_allow_attribute() {
    let v = plain(
        r#"(() => {
            const f = document.createElement("iframe");
            f.setAttribute("allow", "camera 'src'; geolocation 'none'");
            f.setAttribute("src", "/child.html");
            document.body.appendChild(f);
            const k = f.featurePolicy;
            return {
                tag: Object.prototype.toString.call(k),
                same: k === f.featurePolicy,
                camera: k.getAllowlistForFeature("camera"),
                geo: k.getAllowlistForFeature("geolocation"),
                ar: k.getAllowlistForFeature("browsing-topics"),
                usb: k.getAllowlistForFeature("usb"),
                nAllowed: k.allowedFeatures().length,
            };
        })()"#,
    )
    .await;
    assert_eq!(v["tag"], "[object PermissionsPolicy]");
    assert_eq!(v["same"], true);
    // Chrome 150, measured on the same `allow` with a same-origin `src`: camera keeps the frame's
    // source origin, geolocation is emptied, everything else keeps the document's own default.
    assert_eq!(strings(&v["camera"]), [ORIGIN]);
    assert!(strings(&v["geo"]).is_empty());
    assert_eq!(strings(&v["ar"]), ["*"]);
    assert_eq!(strings(&v["usb"]), [ORIGIN]);
    assert_eq!(v["nAllowed"], 79, "only geolocation is removed");
}
