//! G1.7: functional fetch over a local test server (http:// so no TLS handshake is involved).
use engine::Engine;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[tokio::test]
async fn native_fetch() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 2048];
        let _ = sock.read(&mut buf).await;
        let body = r#"{"hello":"world"}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(), body
        );
        let _ = sock.write_all(resp.as_bytes()).await;
        let _ = sock.flush().await;
    });

    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.eval("fetch", format!(r#"
        fetch("http://{addr}/api").then(async (r) => {{
            print("status=" + r.status);
            print("ok=" + r.ok);
            print("ct=" + r.headers.get("content-type"));
            const j = await r.json();
            print("hello=" + j.hello);
        }});
    "#)).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "status=200",
        "ok=true",
        "ct=application/json",
        "hello=world",
    ]);
}
