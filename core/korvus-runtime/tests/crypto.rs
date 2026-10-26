//! WebCrypto: getRandomValues, randomUUID, subtle.digest(SHA-256).
use engine::Engine;
#[tokio::test]
async fn webcrypto() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.eval("c", r#"
        const a = new Uint8Array(8);
        const r = crypto.getRandomValues(a);
        print("same=" + (r === a) + " len=" + a.length);
        print("uuid=" + /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(crypto.randomUUID()));
        // SHA-256 of "abc" = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
        crypto.subtle.digest("SHA-256", new TextEncoder().encode("abc")).then(buf => {
            const hex = Array.from(new Uint8Array(buf)).map(b => b.toString(16).padStart(2,"0")).join("");
            print("sha=" + hex);
        });
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "same=true len=8",
        "uuid=true",
        "sha=ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    ]);
}
