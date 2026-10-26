//! URLSearchParams / FormData / Blob + URL.searchParams.
use engine::Engine;
#[tokio::test]
async fn form_apis() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.eval("f", r##"
        const sp = new URLSearchParams("a=1&b=2&a=3");
        print("get=" + sp.get("a") + " all=" + sp.getAll("a").join(",") + " has=" + sp.has("b"));
        sp.append("c", "4"); sp.set("a", "9");
        print("str=" + sp.toString());
        const u = new URL("https://x.com/?q=hi&n=2");
        print("usp=" + u.searchParams.get("q") + "," + u.searchParams.get("n"));
        const fd = new FormData();
        fd.append("x", "1"); fd.append("x", "2");
        print("fd=" + fd.getAll("x").join(",") + " has=" + fd.has("x"));
        const b = new Blob(["hello", "!"], { type: "text/plain" });
        print("blob=" + b.size + "," + b.type);
    "##.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "get=1 all=1,3 has=true",
        "str=a=9&b=2&c=4",
        "usp=hi,2",
        "fd=1,2 has=true",
        "blob=6,text/plain",
    ]);
}
