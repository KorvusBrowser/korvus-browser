//! What a page reads of its WebGL device. Owner 2026-10-12: WebGL draws on THIS PC's GPU, as Chrome's
//! does, so the persona's `deviceOracle.webgl` recording takes the device's own values. Owner 2026-10-17:
//! a Linux host wears a WINDOWS persona, and a Windows persona recorded its device through ANGLE's D3D11
//! renderer, which this host has not got -- so for such a persona the recording's limits, precision formats,
//! extension lists and sample counts STAY the recording's (Windows Chrome's), and only `WEBGL_debug_renderer_info`'s
//! two strings name THIS PC's GPU, as Windows Chrome names it. This mirrors the engine's `webgl::with_device` /
//! `with_d3d11_names` exactly, so the tests pin the oracle's path to JS -- not one machine's GPU.

/// The `webgl` block a page under the persona whose `deviceOracle` is `device_oracle_json` reads;
/// `Value::Null` when it has no WebGL (no usable GPU, or a persona recorded without one).
pub fn page_webgl(device_oracle_json: &str) -> serde_json::Value {
    let oracle: serde_json::Value = serde_json::from_str(device_oracle_json).expect("the oracle block parses");
    let mut webgl = oracle["webgl"].clone();
    let Some(device) = render::gpu::webgl_block() else { return serde_json::Value::Null };
    let device: serde_json::Value = serde_json::from_str(&device).expect("the device block parses");
    let (Some(w), Some(d)) = (webgl.as_object_mut(), device.as_object()) else { return webgl };

    // Was the persona recorded through ANGLE's D3D11 renderer -- a Windows persona's? (engine: `recorded_is_d3d11`)
    let is_d3d11 = w
        .get("unmaskedRenderer")
        .and_then(|r| r.as_str())
        .is_some_and(|r| r.contains("Direct3D11"));

    if is_d3d11 {
        // Windows persona: keep the recording's device values; name only the GPU, D3D11-style, from the
        // device's `_d3d11Vendor` / `_d3d11Renderer` (engine: `with_d3d11_names`).
        let (Some(vendor), Some(renderer)) = (d.get("_d3d11Vendor").cloned(), d.get("_d3d11Renderer").cloned()) else {
            return webgl;
        };
        for (name, value, pname) in [("unmaskedVendor", vendor, "37445"), ("unmaskedRenderer", renderer, "37446")] {
            w.insert(name.into(), value.clone());
            for params in ["paramsByNum", "paramsByNumV2"] {
                if let Some(p) = w.get_mut(params).and_then(|p| p.as_object_mut()) {
                    if p.contains_key(pname) {
                        p.insert(pname.into(), value.clone());
                    }
                }
            }
        }
    } else {
        // Same-OS persona on its own GPU: the device's own values replace the recording's, bar the device's
        // `_`-prefixed notes for other paths (engine: the non-d3d11 branch of `with_device`).
        for (k, v) in d {
            if k.starts_with('_') {
                continue;
            }
            w.insert(k.clone(), v.clone());
        }
    }
    webgl
}
