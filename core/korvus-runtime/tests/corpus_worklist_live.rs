//! The corpus's ranked `stub_hits` worklist, item by item.
//!
//! `tools/corpus/run.py` records which UNIMPLEMENTED members each site actually CALLED. These are the
//! members from a 2026-09-11 corpus run that this unit closed, each with the value
//! Chrome 151 headless gave for it over a loopback server on 2026-09-11.

use engine::Engine;

async fn probe(html: &str, js: &str) -> Vec<String> {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(html).unwrap();
    e.eval("i", js.into()).unwrap();
    e.run().await.unwrap();
    e.take_output()
}

/// `Document.fragmentDirective` (`vuejs.org`, `vitejs.dev`) answered `null`, which is not a
/// `FragmentDirective`. The interface is genuinely EMPTY -- Chrome's prototype carries nothing but
/// `constructor` and the instance has no own properties -- so existing, wearing that prototype and
/// being `[SameObject]` is the whole of what a page can observe, and all three are real here.
#[tokio::test]
async fn document_fragment_directive_is_an_object() {
    let out = probe(
        r#"<!doctype html><html><body></body></html>"#,
        r#"
        print("str=" + String(document.fragmentDirective));
        print("ctor=" + document.fragmentDirective.constructor.name);
        print("proto=" + JSON.stringify(Object.getOwnPropertyNames(Object.getPrototypeOf(document.fragmentDirective))));
        print("own=" + JSON.stringify(Object.getOwnPropertyNames(document.fragmentDirective)));
        print("same=" + (document.fragmentDirective === document.fragmentDirective));
        print("proto-is=" + (Object.getPrototypeOf(document.fragmentDirective) === FragmentDirective.prototype));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec!["str=[object FragmentDirective]", "ctor=FragmentDirective", "proto=[\"constructor\"]", "own=[]", "same=true", "proto-is=true"]
    );
}

/// `Response.arrayBuffer` (`vitejs.dev`, `news.google.com`) and `Response.bytes`: the body as BYTES,
/// which here is the UTF-8 encoding of the string the class stored. The probe body is
/// `{"k":"v\u{e9}"}` -- ten CHARACTERS and eleven BYTES, so a length that came from the string rather
/// than from the encoding would read 10. Chrome reads 11.
#[tokio::test]
async fn response_array_buffer_and_bytes_are_the_utf8_body() {
    let out = probe(
        r#"<!doctype html><html><body></body></html>"#,
        r#"
        const body = '{"k":"vé"}';
        print("chars=" + body.length);
        const r = new Response(body);
        r.arrayBuffer().then(function (ab) {
            print("ab-ctor=" + ab.constructor.name);
            print("ab-len=" + ab.byteLength);
            print("ab-first=" + new Uint8Array(ab)[0]);
        });
        new Response(body).bytes().then(function (b) {
            print("bytes-ctor=" + b.constructor.name);
            print("bytes-len=" + b.length);
            print("bytes-first=" + b[0]);
        });
        new Response("").arrayBuffer().then(function (ab) { print("empty=" + ab.byteLength); });
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "chars=10",
            "ab-ctor=ArrayBuffer",
            // ELEVEN: the e-acute is two bytes.
            "ab-len=11",
            // '{'
            "ab-first=123",
            "bytes-ctor=Uint8Array",
            "bytes-len=11",
            "bytes-first=123",
            "empty=0",
        ]
    );
}

/// `Response.clone` (`rakuten`, `news.google.com`): a SECOND Response over the same body, carrying
/// `url`, `type` and `redirected` -- which `new Response(body, init)` cannot express, and which is why
/// this copies the internal slots instead of reconstructing.
#[tokio::test]
async fn response_clone_carries_every_slot() {
    let out = probe(
        r#"<!doctype html><html><body></body></html>"#,
        r#"
        const r = new Response("hello", { status: 201, statusText: "Created" });
        const c = r.clone();
        print("is=" + String(c));
        print("distinct=" + (c !== r));
        print("proto=" + (Object.getPrototypeOf(c) === Response.prototype));
        print("status=" + c.status + " ok=" + c.ok + " statusText=" + c.statusText);
        print("own=" + JSON.stringify(Object.getOwnPropertyNames(c).filter(function (n) { return n[0] !== "_"; })));
        c.text().then(function (t) { print("text=" + t); });
        r.text().then(function (t) { print("orig-still-readable=" + t); });
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "is=[object Response]",
            "distinct=true",
            "proto=true",
            // 201 IS in [200,300), so `ok` is true -- Chrome agrees, and an earlier draft of this
            // test did not.
            "status=201 ok=true statusText=Created",
            // A Response has no own properties a page can see.
            "own=[]",
            "text=hello",
            // Cloning does not consume the original.
            "orig-still-readable=hello",
        ]
    );
}

/// `Response.blob()`: the body as a real `Blob`, sized in BYTES and typed by the response's own
/// `Content-Type`.
#[tokio::test]
async fn response_blob_is_sized_in_bytes_and_typed_by_content_type() {
    let out = probe(
        r#"<!doctype html><html><body></body></html>"#,
        r#"
        new Response('{"k":"vé"}', { headers: { "content-type": "application/json" } }).blob().then(function (b) {
            print("ctor=" + b.constructor.name);
            print("size=" + b.size);
            print("type=" + b.type);
        });
        "#,
    )
    .await;
    // Chrome, the same three expressions on a UTF-8 page: Blob / 11 / "application/json".
    //
    // The UNTYPED case is deliberately not here: Chrome gives `new Response("x")` a default
    // `Content-Type: text/plain;charset=UTF-8` and this engine's Response CLASS
    // (`js/shared-globals.js`) sets no headers at all, so `blob().type` is "" -- a gap in the class,
    // not in `blob()`, and not this unit's to close (the rules forbid growing that file).
    assert_eq!(out, vec!["ctor=Blob", "size=11", "type=application/json"]);
}
