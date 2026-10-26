//! XMLHttpRequest over the native fetch op (against a local server).
use engine::Engine;
use tokio::io::{AsyncReadExt, AsyncWriteExt};#[tokio::test]
async fn xml_http_request() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut b = [0u8; 1024]; let _ = s.read(&mut b).await;
        let body = "hello-xhr";
        let resp = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        let _ = s.write_all(resp.as_bytes()).await; let _ = s.flush().await;
    });
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.eval("x", format!(r#"
        const xhr = new XMLHttpRequest();
        xhr.open("GET", "http://{addr}/");
        xhr.onload = () => print("status=" + xhr.status + " body=" + xhr.responseText + " rs=" + xhr.readyState);
        xhr.send();
    "#)).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec!["status=200 body=hello-xhr rs=4"]);
}

/// A response with no body bytes never enters LOADING: HEADERS_RECEIVED -> DONE, readyState [1,2,4]
/// (XHR "process response body chunk" is what sets LOADING). Chrome 150, measured 2026-10-07 on a POST
/// answered 204 and on an empty 200; korvus fired [1,2,3,4] (korvus-analysis U-6).
#[tokio::test]
async fn an_empty_response_skips_loading() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for status in ["204 No Content", "200 OK"] {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut b = [0u8; 2048];
            let _ = s.read(&mut b).await;
            let resp = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            let _ = s.write_all(resp.as_bytes()).await;
            let _ = s.flush().await;
        }
    });
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.eval("x", format!(r#"
        function go(method, then) {{
            const seen = [];
            const xhr = new XMLHttpRequest();
            xhr.onreadystatechange = () => seen.push(xhr.readyState);
            xhr.open(method, "http://{addr}/");
            xhr.onloadend = () => {{ print(method + " " + JSON.stringify(seen)); then(); }};
            xhr.send(method === "POST" ? "{{}}" : null);
        }}
        go("POST", () => go("GET", () => {{}}));
    "#)).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec!["POST [1,2,4]", "GET [1,2,4]"]);
}
