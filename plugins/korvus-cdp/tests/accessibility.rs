//! The accessibility tree over CDP, and the whole DOM tree, as Chrome 150 answers them.
//!
//! Every `Accessibility.*` method was `-32601`, and `DOM.getDocument` answered the document alone whatever the
//! `depth`: Stagehand -- whose `act`, `observe` and `extract` read a page as `Accessibility.getFullAXTree` merged with
//! `DOM.getDocument({depth: -1, pierce: true})` -- could not read a page at all, nor could Puppeteer's
//! `page.accessibility.snapshot()` or its `aria/` selectors (`queryAXTree`).
//!
//! `fixtures/accessibility.chrome-150.json` is stock Chrome 150.0.7871.46's answer: headless, `--lang=en-US`, a fresh
//! profile, one raw CDP client, on `fixtures/accessibility.html` (2026-10-28; `name.sources` stripped). Graded:
//! * the tree a client sees -- each node that is not ignored and is made for a node of the page's own DOM -- as its
//!   depth, role, name, value, description and properties, in document order;
//! * `getPartialAXTree` on `#q`, `queryAXTree` by name and role and by role, `getFullAXTree` with a `depth`, and the
//!   tree of a frame by its `frameId`;
//! * `DOM.getDocument` with `depth: -1, pierce: true` (every node, its name, type, value and attributes, shadow roots
//!   and the frame's document) and with no depth (the document, `<html>`, its head and body).
//!
//! The worn browser's UI language names a reset button and a file input's no-file text, in Chrome as here (Chrome 150
//! under a Korean UI: "초기화", "선택된 파일 없음"); the recording is en-US, so its two labels are read in the shipped
//! persona's language before they are compared.
//!
//! KNOWN, not graded -- this engine has no node for them: the user-agent shadow trees Chrome builds inside a form
//! control (a text field's `generic` editor, the `StaticText` of a submit button's label, a `<select>`'s popup and a
//! `<details>`' slots), `::marker` pseudo-elements, and an `InlineTextBox` per laid-out line. korvus lists a
//! `MenuListPopup`, a list marker and a button's label as nodes with no `backendDOMNodeId`, and one `InlineTextBox`
//! per text node.
use engine::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

type ClientWs = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const PAGE: &str = include_str!("fixtures/accessibility.html");
const CHROME: &str = include_str!("fixtures/accessibility.chrome-150.json");

async fn fixture_server(page: &'static str) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else { return };
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf).await;
                let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}", page.len());
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.flush().await;
            });
        }
    });
    port
}

struct Client {
    ws: ClientWs,
    next: i64,
    events: Vec<Value>,
}

impl Client {
    /// One command: its whole answer (`result` or `error`).
    async fn call(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        self.ws.send(Message::Text(json!({ "id": id, "method": method, "params": params }).to_string())).await.unwrap();
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(30), self.ws.next()).await.expect("an answer within 30 s").unwrap().unwrap();
            if let Message::Text(t) = msg {
                let v: Value = serde_json::from_str(&t).unwrap();
                if v.get("id").and_then(Value::as_i64) == Some(id) {
                    return v;
                }
                if v.get("method").is_some() {
                    self.events.push(v);
                }
            }
        }
    }

    async fn wait_for(&mut self, method: &str) {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(15) && !self.events.iter().any(|e| e["method"] == method) {
            if let Ok(Some(Ok(Message::Text(t)))) = tokio::time::timeout(Duration::from_millis(200), self.ws.next()).await {
                let v: Value = serde_json::from_str(&t).unwrap();
                if v.get("method").is_some() {
                    self.events.push(v);
                }
            }
        }
    }
}

/// The backend ids of the page's own DOM nodes in a `DOM.getDocument` tree: no user-agent shadow tree, no
/// pseudo-element.
fn page_dom(n: &Value, out: &mut HashSet<i64>) {
    out.insert(n["backendNodeId"].as_i64().unwrap());
    for c in n["children"].as_array().into_iter().flatten() {
        page_dom(c, out);
    }
    for r in n["shadowRoots"].as_array().into_iter().flatten() {
        if r["shadowRootType"] != "user-agent" {
            page_dom(r, out);
        }
    }
    if n.get("contentDocument").is_some() {
        page_dom(&n["contentDocument"], out);
    }
}

fn host_free(v: &Value) -> String {
    let s = match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let mut out = String::new();
    let mut rest = s.as_str();
    while let Some(i) = rest.find("http://127.0.0.1:") {
        out.push_str(&rest[..i]);
        out.push_str("http://HOST");
        rest = rest[i + "http://127.0.0.1:".len()..].trim_start_matches(|c: char| c.is_ascii_digit());
    }
    out.push_str(rest);
    out
}

/// The tree a client sees, one line a node, indented by its depth among such nodes: role, name, value, description,
/// properties (a relation by its name alone).
fn seen(nodes: &[Value], page: &HashSet<i64>) -> Vec<String> {
    let by: std::collections::HashMap<&str, &Value> = nodes.iter().map(|n| (n["nodeId"].as_str().unwrap(), n)).collect();
    let mut out = Vec::new();
    fn walk(by: &std::collections::HashMap<&str, &Value>, page: &HashSet<i64>, id: &str, depth: usize, out: &mut Vec<String>) {
        let Some(n) = by.get(id) else { return };
        let name = n["name"]["value"].as_str().unwrap_or("");
        let shown = n["ignored"] == false
            && n["backendDOMNodeId"].as_i64().is_some_and(|b| page.contains(&b))
            && !(n["role"]["value"] == "StaticText" && name.trim().is_empty());
        if shown {
            let mut line = format!("{}{} {:?}", "  ".repeat(depth), n["role"]["value"].as_str().unwrap(), name);
            if n.get("value").is_some() {
                line.push_str(&format!(" value={}", host_free(&n["value"]["value"])));
            }
            if n.get("description").is_some() {
                line.push_str(&format!(" desc={:?}", n["description"]["value"].as_str().unwrap_or("")));
            }
            let props: Vec<String> = n["properties"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|p| if p["value"].get("relatedNodes").is_some() { p["name"].as_str().unwrap().to_string() } else { format!("{}={}", p["name"].as_str().unwrap(), host_free(&p["value"]["value"])) })
                .collect();
            line.push_str(&format!(" [{}]", props.join(" ")));
            out.push(line);
        }
        for c in n["childIds"].as_array().into_iter().flatten() {
            walk(by, page, c.as_str().unwrap(), depth + usize::from(shown), out);
        }
    }
    walk(&by, page, nodes[0]["nodeId"].as_str().unwrap(), 0, &mut out);
    out
}

/// A `DOM.getDocument` tree as lines: each node of the page's own DOM by its depth, type, name, value and attributes.
fn dom_lines(n: &Value, depth: usize, out: &mut Vec<String>) {
    let attrs = n["attributes"].as_array().map(|a| a.iter().map(|v| v.as_str().unwrap().to_string()).collect::<Vec<_>>().join(",")).unwrap_or_default();
    let value = n["nodeValue"].as_str().unwrap_or("");
    out.push(format!("{}{} {} {:?} [{}] {}", "  ".repeat(depth), n["nodeType"], n["nodeName"].as_str().unwrap(), value.chars().take(40).collect::<String>(), attrs, n["childNodeCount"]));
    for r in n["shadowRoots"].as_array().into_iter().flatten() {
        if r["shadowRootType"] != "user-agent" {
            out.push(format!("{}shadow {}", "  ".repeat(depth + 1), r["shadowRootType"]));
            dom_lines(r, depth + 2, out);
        }
    }
    for c in n["children"].as_array().into_iter().flatten() {
        dom_lines(c, depth + 1, out);
    }
    if n.get("contentDocument").is_some() {
        out.push(format!("{}contentDocument", "  ".repeat(depth + 1)));
        dom_lines(&n["contentDocument"], depth + 2, out);
    }
}

/// `want` and `got` line by line, the first lines that differ shown with their neighbours.
fn same_lines(what: &str, want: &[String], got: &[String]) {
    if want == got {
        return;
    }
    let first = want.iter().zip(got).position(|(a, b)| a != b).unwrap_or(want.len().min(got.len()));
    let from = first.saturating_sub(3);
    panic!(
        "{what}: {} lines from Chrome, {} from korvus, first difference at line {first}\n--- chrome\n{}\n--- korvus\n{}",
        want.len(),
        got.len(),
        want[from..(first + 6).min(want.len())].join("\n"),
        got[from..(first + 6).min(got.len())].join("\n"),
    );
}

#[tokio::test]
async fn the_accessibility_tree_and_the_dom_tree_are_chromes() {
    // The request ledger is process-global: a navigating test takes its turn (see `navigation::ONE_AT_A_TIME`).
    let _turn = crate::navigation::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    // read for the whole test, inside the client future too
    let chrome: &'static Value = Box::leak(Box::new(serde_json::from_str(CHROME).expect("the recording is JSON")));
    let mut chrome_page = HashSet::new();
    page_dom(&chrome["document"], &mut chrome_page);
    let chrome_nodes = chrome["full"].as_array().unwrap();
    let by_id = |id: &Value| chrome_nodes.iter().find(|n| n["nodeId"] == *id).unwrap();

    let port = fixture_server(PAGE).await;
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let server = cdp::serve_connection(l, Engine::new(Box::new(persona::CapturedPersona::default())));
    let client = async move {
        let ws = tokio_tungstenite::connect_async(format!("ws://{addr}/devtools/page/x")).await.unwrap().0;
        let mut c = Client { ws, next: 0, events: Vec::new() };
        c.call("Page.enable", json!({})).await;
        c.call("Runtime.enable", json!({})).await;
        c.call("Page.navigate", json!({ "url": format!("http://127.0.0.1:{port}/accessibility.html") })).await;
        c.wait_for("Page.loadEventFired").await;

        assert_eq!(c.call("Accessibility.enable", json!({})).await["result"], json!({}));
        let doc = c.call("DOM.getDocument", json!({ "depth": -1, "pierce": true })).await;
        let root = &doc["result"]["root"];
        let (mut want, mut got) = (Vec::new(), Vec::new());
        dom_lines(&chrome["document"], 0, &mut want);
        dom_lines(root, 0, &mut got);
        same_lines("DOM.getDocument depth -1 pierce", &want, &got);
        let mut page = HashSet::new();
        page_dom(root, &mut page);

        let full = c.call("Accessibility.getFullAXTree", json!({})).await;
        let nodes = full["result"]["nodes"].as_array().unwrap_or_else(|| panic!("a tree: {full}")).clone();
        let theme = spec::Persona::theme(&persona::CapturedPersona::default()).expect("the shipped persona has a theme");
        let no_file = if theme.submit_label == "제출" { "선택된 파일 없음" } else { "No file chosen" };
        let want: Vec<String> = seen(chrome_nodes, &chrome_page)
            .into_iter()
            .map(|l| l.replace("button \"Reset\"", &format!("button \"{}\"", if theme.reset_label.is_empty() { "Reset" } else { theme.reset_label })).replace("value=No file chosen", &format!("value={no_file}")))
            .collect();
        same_lines("getFullAXTree, the tree a client sees", &want, &seen(&nodes, &page));
        assert_eq!(nodes[0]["role"]["value"], "RootWebArea");
        assert_eq!(nodes[0]["name"]["value"], "Accessibility Fixture");
        assert!(nodes[0]["frameId"].is_string(), "the root names its frame: {}", nodes[0]);
        let ids: HashSet<&str> = nodes.iter().map(|n| n["nodeId"].as_str().unwrap()).collect();
        for n in &nodes {
            for c in n["childIds"].as_array().unwrap() {
                assert!(ids.contains(c.as_str().unwrap()), "every child is listed: {c} of {}", n["nodeId"]);
            }
        }

        // the levels a client sees: Chrome's depth 2 is the root, the landmarks and what is in them
        let shallow = c.call("Accessibility.getFullAXTree", json!({ "depth": 2 })).await;
        let role_name = |n: &Value| format!("{} {:?}", n["role"]["value"].as_str().unwrap(), n["name"]["value"].as_str().unwrap_or(""));
        let want: Vec<String> = chrome["full_depth2"].as_array().unwrap().iter().map(by_id).filter(|n| n["ignored"] == false && n["backendDOMNodeId"].as_i64().is_some_and(|b| chrome_page.contains(&b))).map(role_name).collect();
        let got: Vec<String> = shallow["result"]["nodes"].as_array().unwrap().iter().filter(|n| n["ignored"] == false && n["backendDOMNodeId"].as_i64().is_some_and(|b| page.contains(&b))).map(role_name).collect();
        same_lines("getFullAXTree depth 2", &want, &got);

        let q = c.call("Runtime.evaluate", json!({ "expression": "document.querySelector('#q')" })).await;
        let q_id = q["result"]["result"]["objectId"].as_str().unwrap().to_string();
        let partial = c.call("Accessibility.getPartialAXTree", json!({ "objectId": q_id })).await;
        let want: Vec<String> = chrome["partial_q"].as_array().unwrap().iter().map(by_id).filter(|n| n["backendDOMNodeId"].as_i64().is_some_and(|b| chrome_page.contains(&b))).map(role_name).collect();
        let got: Vec<String> = partial["result"]["nodes"].as_array().unwrap().iter().filter(|n| n["backendDOMNodeId"].as_i64().is_some_and(|b| page.contains(&b))).map(role_name).collect();
        same_lines("getPartialAXTree #q: the node, then its ancestors", &want, &got);
        let alone = c.call("Accessibility.getPartialAXTree", json!({ "objectId": q_id, "fetchRelatives": false })).await;
        assert_eq!(alone["result"]["nodes"].as_array().map(Vec::len), Some(1), "without relatives, the node alone: {alone}");

        let document = c.call("Runtime.evaluate", json!({ "expression": "document" })).await;
        let doc_id = document["result"]["result"]["objectId"].as_str().unwrap().to_string();
        let claim = c.call("Accessibility.queryAXTree", json!({ "objectId": doc_id, "accessibleName": "Claim", "role": "button" })).await;
        let got: Vec<String> = claim["result"]["nodes"].as_array().unwrap().iter().map(role_name).collect();
        let want: Vec<String> = chrome["query_button_claim"].as_array().unwrap().iter().map(by_id).map(role_name).collect();
        same_lines("queryAXTree button Claim", &want, &got);
        let links = c.call("Accessibility.queryAXTree", json!({ "objectId": doc_id, "role": "link" })).await;
        let got: Vec<String> = links["result"]["nodes"].as_array().unwrap().iter().map(role_name).collect();
        let want: Vec<String> = chrome["query_role_link"].as_array().unwrap().iter().map(by_id).map(role_name).collect();
        same_lines("queryAXTree role link", &want, &got);

        let root = c.call("Accessibility.getRootAXNode", json!({})).await;
        assert_eq!(root["result"]["node"]["role"]["value"], "RootWebArea", "{root}");

        // a frame's own tree, by its id
        let frames = c.call("Page.getFrameTree", json!({})).await;
        let child = frames["result"]["frameTree"]["childFrames"][0]["frame"]["id"].as_str().unwrap_or_else(|| panic!("the iframe's frame: {frames}")).to_string();
        let inner = c.call("Accessibility.getFullAXTree", json!({ "frameId": child })).await;
        let got: Vec<String> = inner["result"]["nodes"].as_array().unwrap_or_else(|| panic!("the frame's tree: {inner}")).iter().filter(|n| n["ignored"] == false && n["role"]["value"] != "InlineTextBox").map(role_name).collect();
        assert_eq!(got, ["RootWebArea \"\"", "button \"In frame\"", "StaticText \"In frame\""], "{inner}");
        let none = c.call("Accessibility.getFullAXTree", json!({ "frameId": "NOSUCHFRAME" })).await;
        assert_eq!(none["error"]["message"], "Frame with the given frameId is not found.", "{none}");

        // no depth: the document, its doctype and `<html>`, and `<html>`'s head and body
        let shallow_doc = c.call("DOM.getDocument", json!({})).await;
        let (mut want, mut got) = (Vec::new(), Vec::new());
        dom_lines(&chrome["document_shallow"], 0, &mut want);
        dom_lines(&shallow_doc["result"]["root"], 0, &mut got);
        same_lines("DOM.getDocument with no depth", &want, &got);

        let none = c.call("DOM.querySelector", json!({ "nodeId": shallow_doc["result"]["root"]["nodeId"], "selector": "#nothing" })).await;
        assert_eq!(none["result"]["nodeId"], 0, "no match is node 0: {none}");
        let found = c.call("DOM.querySelectorAll", json!({ "nodeId": shallow_doc["result"]["root"]["nodeId"], "selector": "nav a" })).await;
        assert_eq!(found["result"]["nodeIds"].as_array().map(Vec::len), Some(2), "{found}");
        let bad = c.call("DOM.querySelector", json!({ "nodeId": shallow_doc["result"]["root"]["nodeId"], "selector": "[[" })).await;
        assert_eq!(bad["error"]["message"], "DOM Error while querying", "{bad}");
        let _ = c.ws.close(None).await;
    };
    let (served, ()) = tokio::join!(server, client);
    served.expect("the connection is served");
}

/// Each element's `id` by its backend id, in a `DOM.getDocument` tree.
fn element_ids(n: &Value, out: &mut HashMap<i64, String>) {
    if let Some(a) = n["attributes"].as_array() {
        for kv in a.chunks(2) {
            if kv[0] == "id" {
                out.insert(n["backendNodeId"].as_i64().unwrap(), kv[1].as_str().unwrap().to_string());
            }
        }
    }
    for c in n["children"].as_array().into_iter().flatten() {
        element_ids(c, out);
    }
}

/// A control's name from its `<label>`: the label's text as its line shows it, never trimmed.
///
/// korvus trimmed it for every control but a checkbox or radio. Chrome 152 (`fixtures/labels.chrome-152.json`:
/// 152.0.7977.82, headless, `--lang=en-US`, a fresh profile, `Accessibility.getFullAXTree` on `fixtures/labels.html`,
/// each control's role and name by its `id`) keeps what its line keeps: `<label>Search <input></label>` names the
/// textbox `"Search "`, `<label> Name <input> </label>` `"Name "`, `<label><input> after</label>` `" after"`, a label
/// that ends its block `"Two lines"`. chrome-devtools-mcp's snapshot shows the name as it is. What the label holds
/// beyond text is read as the tree holds it: a space beside the control is none (`<input> <strong>Bold</strong> text`:
/// `"Bold text"`), a block a space between its neighbours but none at the edges (`<input><div>B</div>`: `"B"`), a
/// `<br>` a space even there (`<br>B `: `" B "`).
#[tokio::test]
async fn a_label_names_its_control_as_its_line_shows_it() {
    let _turn = crate::navigation::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let chrome: Value = serde_json::from_str(include_str!("fixtures/labels.chrome-152.json")).expect("the recording is JSON");
    let port = fixture_server(include_str!("fixtures/labels.html")).await;
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let server = cdp::serve_connection(l, Engine::new(Box::new(persona::CapturedPersona::default())));
    let client = async move {
        let ws = tokio_tungstenite::connect_async(format!("ws://{addr}/devtools/page/x")).await.unwrap().0;
        let mut c = Client { ws, next: 0, events: Vec::new() };
        c.call("Page.enable", json!({})).await;
        c.call("Page.navigate", json!({ "url": format!("http://127.0.0.1:{port}/labels.html") })).await;
        c.wait_for("Page.loadEventFired").await;
        let doc = c.call("DOM.getDocument", json!({ "depth": -1 })).await;
        let mut ids = HashMap::new();
        element_ids(&doc["result"]["root"], &mut ids);
        let full = c.call("Accessibility.getFullAXTree", json!({})).await;
        let mut got = serde_json::Map::new();
        for n in full["result"]["nodes"].as_array().unwrap_or_else(|| panic!("a tree: {full}")) {
            if let Some(id) = n["backendDOMNodeId"].as_i64().and_then(|b| ids.get(&b)) {
                got.insert(id.clone(), json!([n["role"]["value"], n["name"]["value"]]));
            }
        }
        c.ws.send(Message::Close(None)).await.unwrap();
        got
    };
    let (server, got) = tokio::join!(server, client);
    server.unwrap();
    let mut wrong = Vec::new();
    for (id, want) in chrome.as_object().unwrap() {
        if got.get(id) != Some(want) {
            wrong.push(format!("#{id}: Chrome {want}, korvus {}", got.get(id).cloned().unwrap_or_default()));
        }
    }
    assert!(wrong.is_empty(), "{} of {} controls named otherwise:\n{}", wrong.len(), chrome.as_object().unwrap().len(), wrong.join("\n"));
}

/// An element without a box, a clickable one, and a checkbox's label, as Chrome lists them.
///
/// Stagehand's snapshots (D185's A/B) showed three ways korvus's tree was not Chrome's. A `display: contents` element
/// was left out with its role: Tailwind's `<form class="contents">` lost its `form`, though Chrome 152 keeps every
/// role but a paragraph's (which needs what keeps a generic). An `<a>` with no `href` stayed generic with a click
/// listener, where Chrome makes it a link, and a generic element with one is a node of its own (Blink's
/// `IsClickable`: `click`, `mousedown`, `mouseup`, a listener or an `on…` attribute; not a parent's, not `keydown`).
/// And a checkbox's `<label>` was always left out, where Chrome keeps it as a `LabelText` round the control once it
/// holds more than text (YC's company filters: `<label><input type=checkbox><span>`). `fixtures/ax-structure.chrome-152.json`
/// is Chrome 152.0.7977.82's tree under each case's `<div id="w-…">` (headless, `--lang=en-US`, a fresh profile):
/// `role:"name"[children]`, an ignored node's role in parentheses.
#[tokio::test]
async fn an_element_without_a_box_or_with_a_click_listener_is_listed_as_chromes_is() {
    let _turn = crate::navigation::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let chrome: Value = serde_json::from_str(include_str!("fixtures/ax-structure.chrome-152.json")).expect("the recording is JSON");
    let port = fixture_server(include_str!("fixtures/ax-structure.html")).await;
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let server = cdp::serve_connection(l, Engine::new(Box::new(persona::CapturedPersona::default())));
    let client = async move {
        let ws = tokio_tungstenite::connect_async(format!("ws://{addr}/devtools/page/x")).await.unwrap().0;
        let mut c = Client { ws, next: 0, events: Vec::new() };
        c.call("Page.enable", json!({})).await;
        c.call("Page.navigate", json!({ "url": format!("http://127.0.0.1:{port}/ax-structure.html") })).await;
        c.wait_for("Page.loadEventFired").await;
        let doc = c.call("DOM.getDocument", json!({ "depth": -1 })).await;
        let mut ids = HashMap::new();
        element_ids(&doc["result"]["root"], &mut ids);
        let full = c.call("Accessibility.getFullAXTree", json!({})).await;
        c.ws.send(Message::Close(None)).await.unwrap();
        (ids, full)
    };
    let (server, (ids, full)) = tokio::join!(server, client);
    server.unwrap();
    let nodes = full["result"]["nodes"].as_array().unwrap_or_else(|| panic!("a tree: {full}"));
    let by_id: HashMap<&str, &Value> = nodes.iter().filter_map(|n| Some((n["nodeId"].as_str()?, n))).collect();
    fn shown(n: &Value, by_id: &HashMap<&str, &Value>) -> String {
        let role = n["role"]["value"].as_str().unwrap_or_default();
        let mut s = if n["ignored"] == true { format!("({role})") } else { role.to_string() };
        if let Some(name) = n["name"]["value"].as_str().filter(|s| !s.is_empty()) {
            s += &format!(":{}", Value::from(name));
        }
        let kids: Vec<String> = n["childIds"].as_array().into_iter().flatten().filter_map(|c| by_id.get(c.as_str()?)).map(|c| shown(c, by_id)).collect();
        if !kids.is_empty() {
            s += &format!("[{}]", kids.join(" "));
        }
        s
    }
    let mut wrong = Vec::new();
    for (case, want) in chrome.as_object().unwrap() {
        let wrapper = format!("w-{case}");
        let node = nodes.iter().find(|n| n["backendDOMNodeId"].as_i64().and_then(|b| ids.get(&b)) == Some(&wrapper));
        let got = node.map(|n| n["childIds"].as_array().into_iter().flatten().filter_map(|c| by_id.get(c.as_str()?)).map(|c| shown(c, &by_id)).collect::<Vec<_>>().join(" "));
        if got.as_deref() != want.as_str() {
            wrong.push(format!("{case}:\n  Chrome {}\n  korvus {}", want.as_str().unwrap(), got.unwrap_or_else(|| "no node".to_string())));
        }
    }
    assert!(wrong.is_empty(), "{} of {} cases listed otherwise:\n{}", wrong.len(), chrome.as_object().unwrap().len(), wrong.join("\n"));
}
