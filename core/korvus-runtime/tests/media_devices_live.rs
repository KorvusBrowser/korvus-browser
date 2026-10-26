//! `navigator.mediaDevices` answers the PERSONA's devices and constraints.
//!
//! Both lists were constants written into `persona_objects.rs`: `enumerateDevices()` listed
//! `audioinput`, `videoinput` and `audiooutput`, and `getSupportedConstraints()` held 16 keys. The
//! Brave 151 bundle records what its browser answered (`nav.mediaDevices`): one `audiooutput` -- the
//! capture machine has no microphone or camera -- and 36 constraints, which Chrome 150 on this box
//! answers too (fixture `V-media-devices`). The expected values below are read from the bundle, never
//! written here.
use engine::Engine;

fn recorded() -> serde_json::Value {
    let v: serde_json::Value = serde_json::from_str(persona::SHIPPED_VALUES).unwrap();
    v["nav"]["mediaDevices"].clone()
}

async fn page() -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.run().await.unwrap();
    e
}

#[tokio::test]
async fn enumerate_devices_lists_the_kinds_the_bundle_recorded() {
    let want: Vec<String> = recorded()["kinds"].as_array().unwrap().iter().map(|k| k.as_str().unwrap().to_string()).collect();
    assert!(!want.is_empty(), "the bundle records at least one device");
    let mut e = page().await;
    e.eval(
        "devices",
        "navigator.mediaDevices.enumerateDevices().then(d => { globalThis.K = d.map(x => x.kind + '|' + x.deviceId + '|' + x.label).join(','); })".to_string(),
    )
    .unwrap();
    e.run().await.unwrap();
    let got = e.eval_value("read", "String(globalThis.K)".to_string()).unwrap();
    let want = want.iter().map(|k| format!("{k}||")).collect::<Vec<_>>().join(",");
    assert_eq!(got.as_str(), Some(want.as_str()), "one entry per recorded kind, every identifying field blank");
}

#[tokio::test]
async fn supported_constraints_are_the_bundles_in_dictionary_order() {
    let rec = recorded();
    let mut want: Vec<String> = rec["supported"].as_object().unwrap().iter().filter(|(_, on)| on.as_bool() == Some(true)).map(|(k, _)| k.clone()).collect();
    // A WebIDL dictionary converts with its members in lexicographic order.
    want.sort();
    assert_eq!(want.len(), 36, "the Brave 151 bundle records 36");
    let mut e = page().await;
    let got = e
        .eval_value(
            "constraints",
            "(() => { const c = navigator.mediaDevices.getSupportedConstraints(); return Object.keys(c).join(',') + '|' + Object.values(c).every(v => v === true); })()"
                .to_string(),
        )
        .unwrap();
    assert_eq!(got.as_str().map(str::to_string), Some(format!("{}|true", want.join(","))));
}
