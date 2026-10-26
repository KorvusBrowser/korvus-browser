//! Ported an agent-browser reference Web-API conformance fixtures pass on Korvus.
//! Each fixture under tests/fixtures/an agent-browser reference/ is a real an agent-browser reference test using its
//! `testing.expectEqual` harness; the driver runs its inline <script>s and asserts.
use engine::Engine;


const TESTING_JS: &str = include_str!("fixtures/conformance/testing.js");
const DRIVER_JS: &str = include_str!("fixtures/conformance/driver.js");
// External worker scripts referenced by fixtures (e.g. `new Worker('./performance-worker.js')`),
// registered so the same-isolate Worker can resolve them by URL.
const PERF_WORKER: &str = include_str!("fixtures/conformance/performance-worker.js");
const CRYPTO_WORKER: &str = include_str!("fixtures/conformance/crypto-worker.js");

async fn run_fixture(html: &str) -> Result<Vec<String>, String> {
    run_fixture_at(html, None).await
}

/// [`run_fixture`] with the document at `url` (`None`: no URL, the engine's `about:blank`).
async fn run_fixture_at(html: &str, url: Option<&str>) -> Result<Vec<String>, String> {
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
    if let Some(u) = url {
        e.set_url(u);
    }
    e.run().await.map_err(|x| x.to_string())?;
    e.load_document(html).map_err(|x| x.to_string())?;
    e.eval("testing.js", TESTING_JS.into()).map_err(|x| x.to_string())?;
    // Register external worker sources by their fixture-relative URL.
    let reg = format!(
        "globalThis.__workerScripts = {{ './performance-worker.js': {perf}, './crypto-worker.js': {crypto} }};",
        perf = serde_json::to_string(PERF_WORKER).unwrap(),
        crypto = serde_json::to_string(CRYPTO_WORKER).unwrap(),
    );
    e.eval("worker-scripts", reg).map_err(|x| x.to_string())?;
    e.run().await.map_err(|x| x.to_string())?;
    e.eval("driver.js", DRIVER_JS.into()).map_err(|x| x.to_string())?;
    // Pump the event loop until the driver prints RESULT:. Worker fixtures now run on REAL isolates and
    // deliver messages from another thread, so a single run() can return (idle) before the reply arrives;
    // loop with a short per-iteration timeout + a deadline. Sync fixtures print RESULT on the first pass.
    let mut out = Vec::new();
    let start = std::time::Instant::now();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if out.iter().any(|l| l.starts_with("RESULT:")) {
            break;
        }
        if start.elapsed() > std::time::Duration::from_secs(20) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    Ok(out)
}

/// A fixture whose premise is an HTTP document -- an agent-browser reference's runner serves its suite from `http://localhost:9582/`
/// -- loaded at such a URL. `history_url_update`'s `history.pushState(null, '', '/test-push-path')` resolves against
/// the document's URL: against `about:blank` it does not parse, and Chrome 150 refuses it as a `SecurityError` (Blink
/// `History::StateObjectAdded` / `CanChangeToUrlForHistoryApi`), where this engine used to make one up
/// (`http://localhost/`) and move there.
macro_rules! fixture_test_at {
    ($name:ident, $file:literal, $url:literal) => {
        #[tokio::test]
        async fn $name() {
            let html = include_str!(concat!("fixtures/conformance/", $file));
            let out = run_fixture_at(html, Some($url)).await.expect("engine ran");
            let result = out.iter().find(|l| l.starts_with("RESULT:")).cloned()
                .unwrap_or_else(|| format!("RESULT:NONE (output: {:?})", out));
            assert_eq!(result, "RESULT:PASS", "fixture {} did not pass; full output: {:?}", $file, out);
        }
    };
}

macro_rules! fixture_test {
    ($name:ident, $file:literal) => {
        #[tokio::test]
        async fn $name() {
            let html = include_str!(concat!("fixtures/conformance/", $file));
            let out = run_fixture(html).await.expect("engine ran");
            let result = out.iter().find(|l| l.starts_with("RESULT:")).cloned()
                .unwrap_or_else(|| format!("RESULT:NONE (output: {:?})", out));
            assert_eq!(result, "RESULT:PASS", "fixture {} did not pass; full output: {:?}", $file, out);
        }
    };
}

// Ported an agent-browser reference conformance fixtures that pass on Korvus (criterion 1). Grows each cycle.
fixture_test!(domexception, "domexception.html");
fixture_test!(css, "css.html");
fixture_test!(document_head_body, "document_head_body.html");
fixture_test!(xmlserializer, "xmlserializer.html");
fixture_test!(processing_instruction, "processing_instruction.html");
fixture_test!(staticrange, "staticrange.html");
fixture_test_at!(history_url_update, "history_url_update.html", "http://localhost:9582/src/browser/tests/history_url_update.html");
fixture_test!(domrect, "domrect.html");
fixture_test!(dompoint, "dompoint.html");
fixture_test!(image_data, "image_data.html");
fixture_test!(data_transfer, "data_transfer.html");
fixture_test!(blob, "blob.html");
fixture_test!(file, "file.html");
fixture_test!(file_reader, "file_reader.html");
fixture_test!(notification, "notification.html");
fixture_test!(message_channel, "message_channel.html");
fixture_test!(range, "range.html");
fixture_test!(selection, "selection.html");
fixture_test!(range_mutations, "range_mutations.html");
fixture_test!(events, "events.html");
fixture_test!(storage, "storage.html");
fixture_test!(dommatrix, "dommatrix.html");
fixture_test!(domparser, "domparser.html");
fixture_test!(domimplementation, "domimplementation.html");
fixture_test!(window_scroll, "window_scroll.html");
fixture_test!(cookie_store, "cookie_store.html");
fixture_test!(performance, "performance.html");
fixture_test!(crypto, "crypto.html");
fixture_test!(indexeddb, "indexeddb.html");
fixture_test!(url, "url.html");
fixture_test!(broadcast_channel, "broadcast_channel.html");
