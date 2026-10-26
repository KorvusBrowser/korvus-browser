//! `window.navigation` on the LIVE surface: the capture's own rows, and the body behind
//! them, which is the session history `history` keeps.
//!
//! The rows are the shipped bundle's oracle (`oracle/Navigation.json`), replayed as recorded. The body
//! assertions are the Navigation API's invariants over that history: one object per entry, a push
//! is a new slot (new key and id), a replacement keeps the slot's key under a new id, `canGoBack`
//! and `index` follow the list `history.length` counts, and a traversal is `history.go`'s.

use engine::Engine;
use std::time::Duration;

const URL: &str = "http://127.0.0.1:8123/page.html";
const DOC: &str = "<!doctype html><html><head><title>t</title></head><body>x</body></html>";

async fn fresh() -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(DOC, URL, &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    e
}

async fn settled(e: &mut Engine, js: &str) -> serde_json::Value {
    e.eval_value_settled("nav", js.to_string(), Duration::from_secs(10)).await.expect("eval")
}

/// Every row the capture recorded for `Navigation`, replayed: a value compared as JSON, a throw by its
/// constructor's name.
#[tokio::test]
async fn the_captures_navigation_rows_hold() {
    let path = persona::shipped_artifact("oracle").join("Navigation.json");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string(&path).expect("oracle/Navigation.json")).unwrap();
    assert!(rows.len() >= 18, "the capture records {} rows for Navigation", rows.len());
    let mut e = fresh().await;
    let mut bad = Vec::new();
    for row in &rows {
        let expr = row["expr"].as_str().unwrap();
        let js = format!(
            r#"(() => {{ try {{ return {{ v: ({expr}) }}; }} catch (err) {{ return {{ t: err && err.constructor ? err.constructor.name : String(err) }}; }} }})()"#
        );
        let got = settled(&mut e, &js).await;
        let ok = match row.get("throws") {
            Some(t) => got.get("t") == Some(t),
            None => got.get("v").unwrap_or(&serde_json::Value::Null) == &row["expected"] && got.get("t").is_none(),
        };
        if !ok {
            bad.push(format!("{expr} -> {got}, recorded {}", row.get("throws").unwrap_or(&row["expected"])));
        }
    }
    assert!(bad.is_empty(), "rows that disagree with the capture:\n{}", bad.join("\n"));
}

/// The entries ARE the session history: a push adds a slot, a replacement renews the id in the same
/// slot, and `index` / `canGoBack` / `entries().length` follow `history.length`.
#[tokio::test]
async fn the_entries_are_the_session_historys() {
    let mut e = fresh().await;
    let v = settled(
        &mut e,
        r##"(() => {
            const n = navigation, c = n.currentEntry, uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
            const fresh = [c.url === location.href, c === n.currentEntry, c === n.entries()[0], n.entries().length,
                           c.index, c.sameDocument, uuid.test(c.key), uuid.test(c.id), c.key !== c.id,
                           n.canGoBack, n.canGoForward, String(c.getState())];
            history.pushState({ a: 1 }, "", "#p");
            const d = n.currentEntry;
            const pushed = [d !== c, d.url === location.href, d.url.endsWith("#p"), c.index, d.index, n.entries().length,
                            history.length, n.canGoBack, n.canGoForward, d.key !== c.key, d.id !== c.id];
            history.replaceState(null, "", "#r");
            const r = n.currentEntry;
            const replaced = [r !== d, r.key === d.key, r.id !== d.id, d.index, r.index, n.entries().length,
                              n.entries()[1] === r, history.length];
            n.updateCurrentEntry({ state: { deep: [1, 2] } });
            const s1 = r.getState(), s2 = r.getState();
            const state = [JSON.stringify(s1), s1 !== s2, String(c.getState()), JSON.stringify(history.state)];
            const act = n.activation;
            const activation = [act === n.activation, act.entry === c, act.from,
                                ["push", "replace", "reload", "traverse"].includes(act.navigationType)];
            return { fresh, pushed, replaced, state, activation };
        })()"##,
    )
    .await;
    assert_eq!(v["fresh"], serde_json::json!([true, true, true, 1, 0, true, true, true, true, false, false, "undefined"]), "a fresh document");
    assert_eq!(v["pushed"], serde_json::json!([true, true, true, 0, 1, 2, 2, true, false, true, true]), "after pushState");
    assert_eq!(v["replaced"], serde_json::json!([true, true, true, -1, 1, 2, true, 2]), "after replaceState");
    // The Navigation API state is not `history.state`, and `getState()` is a fresh copy each call.
    assert_eq!(v["state"], serde_json::json!([r#"{"deep":[1,2]}"#, true, "undefined", "null"]), "updateCurrentEntry");
    assert_eq!(v["activation"], serde_json::json!([true, true, null, true]), "activation");
}

/// `back()` / `forward()` / `traverseTo()` are `history.go`'s traversal, with `{committed, finished}`
/// settling on the destination entry; nowhere to go is the `InvalidStateError` early error.
#[tokio::test]
async fn traversals_are_historys_and_settle_on_their_entry() {
    let mut e = fresh().await;
    let v = settled(
        &mut e,
        r##"(async () => {
            const n = navigation, first = n.currentEntry;
            const why = (p) => p.then(() => "resolved", (err) => err.name + ":" + err.message);
            const none = [await why(n.back().committed), await why(n.forward().finished), await why(n.traverseTo("nope").finished)];
            const self = await n.traverseTo(first.key).finished;
            history.pushState(null, "", "#two");
            const two = n.currentEntry;
            const r = n.back();
            const sync = [n.currentEntry === two, location.hash];
            const landed = await r.finished;
            const back = [landed === first, n.currentEntry === first, location.hash, n.canGoBack, n.canGoForward];
            const again = await n.traverseTo(two.key).committed;
            const to = [again === two, n.currentEntry === two, location.hash];
            return { none, self: self === first, sync, back, to };
        })()"##,
    )
    .await;
    assert_eq!(
        v["none"],
        serde_json::json!(["InvalidStateError:Cannot go back", "InvalidStateError:Cannot go forward", "InvalidStateError:Invalid key"]),
        "no entry to go to"
    );
    assert_eq!(v["self"], true, "the current entry's own key settles at once, on it");
    assert_eq!(v["sync"], serde_json::json!([true, "#two"]), "nothing moves synchronously");
    assert_eq!(v["back"], serde_json::json!([true, true, "", false, true]), "back()");
    assert_eq!(v["to"], serde_json::json!([true, true, "#two"]), "traverseTo(key)");
}
