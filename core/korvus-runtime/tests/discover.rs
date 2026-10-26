//! Discovery: run every ported fixture, print PASS/FAIL (non-asserting map of criterion-1 coverage).
use engine::Engine;
const TESTING_JS: &str = include_str!("fixtures/conformance/testing.js");
const DRIVER_JS: &str = include_str!("fixtures/conformance/driver.js");
async fn run_one(html: &str) -> String {
    let mut e = Engine::new(
        // THE UA HERE IS LOAD-BEARING, not a placeholder. This is an agent-browser reference's own conformance
        // suite, vendored: `testing.js` computes
        //     IS_TEST_RUNNER = navigator.userAgent.startsWith("an agent-browser reference/")
        // and takes a different path when it is false -- one that calls `window.addEventListener`,
        // which several fixtures shadow with a `<script id=addEventListener>`. Handing it the
        // bundle's Chrome UA sent the whole suite down that branch. Everything else about the
        // device still comes from the bundle; only this one string is the harness's.
        persona::Overridden::new().user_agent("Agent/1.0").boxed(),
    );
    if e.run().await.is_err() { return "ERR:boot".into(); }
    if e.load_document(html).is_err() { return "ERR:load".into(); }
    if e.eval("testing.js", TESTING_JS.into()).is_err() { return "ERR:testing".into(); }
    let _ = e.run().await;
    if let Err(x) = e.eval("driver.js", DRIVER_JS.into()) { return format!("ERR:driver:{x}"); }
    let _ = e.run().await;
    e.take_output().into_iter().find(|l| l.starts_with("RESULT:")).unwrap_or_else(|| "RESULT:NONE".into())
}
#[tokio::test]
#[ignore] // discovery/mapping only; run with --ignored
async fn discover() {
    let fixtures: &[(&str, &str)] = &[
        ("blob.html", include_str!("fixtures/conformance/blob.html")),
        ("broadcast_channel.html", include_str!("fixtures/conformance/broadcast_channel.html")),
        ("cookie_store.html", include_str!("fixtures/conformance/cookie_store.html")),
        ("crypto.html", include_str!("fixtures/conformance/crypto.html")),
        ("css.html", include_str!("fixtures/conformance/css.html")),
        ("data_transfer.html", include_str!("fixtures/conformance/data_transfer.html")),
        ("document_head_body.html", include_str!("fixtures/conformance/document_head_body.html")),
        ("domexception.html", include_str!("fixtures/conformance/domexception.html")),
        ("domimplementation.html", include_str!("fixtures/conformance/domimplementation.html")),
        ("dommatrix.html", include_str!("fixtures/conformance/dommatrix.html")),
        ("domparser.html", include_str!("fixtures/conformance/domparser.html")),
        ("dompoint.html", include_str!("fixtures/conformance/dompoint.html")),
        ("domrect.html", include_str!("fixtures/conformance/domrect.html")),
        ("dump.html", include_str!("fixtures/conformance/dump.html")),
        ("events.html", include_str!("fixtures/conformance/events.html")),
        ("file.html", include_str!("fixtures/conformance/file.html")),
        ("file_reader.html", include_str!("fixtures/conformance/file_reader.html")),
        ("history.html", include_str!("fixtures/conformance/history.html")),
        ("history_url_update.html", include_str!("fixtures/conformance/history_url_update.html")),
        ("image_data.html", include_str!("fixtures/conformance/image_data.html")),
        ("indexeddb.html", include_str!("fixtures/conformance/indexeddb.html")),
        ("inject_script.html", include_str!("fixtures/conformance/inject_script.html")),
        ("mcp_actions.html", include_str!("fixtures/conformance/mcp_actions.html")),
        ("mcp_nav.html", include_str!("fixtures/conformance/mcp_nav.html")),
        ("mcp_press_form.html", include_str!("fixtures/conformance/mcp_press_form.html")),
        ("mcp_wait_for_selector.html", include_str!("fixtures/conformance/mcp_wait_for_selector.html")),
        ("message_channel.html", include_str!("fixtures/conformance/message_channel.html")),
        ("notification.html", include_str!("fixtures/conformance/notification.html")),
        ("page_js_error.html", include_str!("fixtures/conformance/page_js_error.html")),
        ("performance.html", include_str!("fixtures/conformance/performance.html")),
        ("processing_instruction.html", include_str!("fixtures/conformance/processing_instruction.html")),
        ("range.html", include_str!("fixtures/conformance/range.html")),
        ("range_mutations.html", include_str!("fixtures/conformance/range_mutations.html")),
        ("selection.html", include_str!("fixtures/conformance/selection.html")),
        ("selector_path.html", include_str!("fixtures/conformance/selector_path.html")),
        ("staticrange.html", include_str!("fixtures/conformance/staticrange.html")),
        ("storage.html", include_str!("fixtures/conformance/storage.html")),
        ("url.html", include_str!("fixtures/conformance/url.html")),
        ("window_scroll.html", include_str!("fixtures/conformance/window_scroll.html")),
        ("xmlserializer.html", include_str!("fixtures/conformance/xmlserializer.html")),
    ];
    let mut pass = 0;
    for (name, html) in fixtures {
        let r = run_one(html).await;
        if r == "RESULT:PASS" { pass += 1; }
        println!("FIXTURE {name} -> {r}");
    }
    println!("SUMMARY {pass}/{} passing", fixtures.len());
}
