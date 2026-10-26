//! WHAT A BINDING HANDS THE PAGE IS CHROME'S ORDINARY OBJECT (found by BrowserScan's investigation). Every
//! `#[serde]` result used to go through deno_core's serde_v8, which makes each struct and map a NULL-prototype object
//! (serde_v8 0.319.0 `ser.rs:215-224`, `:370-380`): `rect.toJSON()`, `point.toJSON()`, `matrix.toJSON()` and
//! `encoder.encodeInto()` had no `Object.prototype`, so BrowserScan's Client Rects component threw on
//! `m.hasOwnProperty(..)` and left its row blank -- the rect values were Chrome's to the bit. The rule now lives in
//! one place, `webapi::binding::idl_value`, which the `#[webapi(own)]` macro calls for every `#[serde]` result (a
//! dictionary or a sequence: the RECEIVER's relevant realm, Blink's `${script_state}`) and every `#[object]` one (a
//! `toJSON()`, Blink's `V8ObjectBuilder`: the CURRENT realm): ordinary objects, members defined with
//! CreateDataProperty in the dictionary's or the builder's order, numbers as Numbers with NaN, the infinities and -0
//! kept. The sweep found three more of the geometry family's own: `DOMRect.toJSON` went through `serde_json::Value`,
//! which made a NaN or an infinity `null`; `DOMRect`'s edges and `DOMQuad.getBounds` skipped a NaN where Blink's
//! `NanSafeMin` / `NanSafeMax` keep it (and settle -0 as `std::min` / `std::max` do); and `DOMQuad.toJSON` read its
//! points through the page-replaceable `p1` getters and wrote them with `[[Set]]`, so a setter on
//! `Object.prototype.p1` swallowed a point.
//!
//! **Expectations read from Google Chrome 150.0.7871.46 on this machine on 2026-10-11** (headed under Xvfb, no
//! DevTools attached, three runs alike): `fixtures/idl-values/sweep-probe.js`, in the `canvas-and-errors` probe page,
//! answered `sweep-chrome-150.json` -- 84 rows over every object an API hands the page that the engine made without a
//! permission or a device: the `toJSON()` of every geometry interface and of the performance entries, dictionaries
//! (`getContextAttributes`, `encodeInto`, `getHighEntropyValues`, `storage.estimate`, `locks.query`, a JWK, ...),
//! sequences, iterator and stream-read results, each one's prototype, keys in order, property attributes and values;
//! the realm each comes from when a frame's function is called on the page's object; and the same in a dedicated
//! worker. The engine before this batch answered 32 of them unlike Chrome.

/// The sweep, row for row.
#[tokio::test]
async fn every_object_a_binding_returns_is_chromes_ordinary_object() {
    let got = crate::chrome_canvas_and_errors_live::answers(crate::chrome_canvas_and_errors_live::Mode::Page, include_str!("fixtures/idl-values/sweep-probe.js")).await;
    crate::chrome_canvas_and_errors_live::assert_as_chrome(got, include_str!("fixtures/idl-values/sweep-chrome-152.json"));
}
