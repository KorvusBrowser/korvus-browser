//! window.location + document.getElementsByTagName.
use engine::Engine;
#[tokio::test]
async fn location_and_tags() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.eval("l0", r#"print("default=" + location.href);"#.into()).unwrap();
    e.set_url("https://ex.com:8443/a/b?x=1#f");
    e.load_html(r#"<!doctype html><html><body><p>1</p><p>2</p><span>3</span></body></html>"#).unwrap();
    e.eval("l1", r#"
        print("href=" + location.href);
        print("proto=" + location.protocol);
        print("host=" + location.host);
        print("path=" + location.pathname);
        print("search=" + location.search);
        print("hash=" + location.hash);
        print("win=" + (window.location === location));
        print("tags=" + Array.from(document.getElementsByTagName("p")).map(el => el.textContent).join(","));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "default=about:blank",
        "href=https://ex.com:8443/a/b?x=1#f",
        "proto=https:",
        "host=ex.com:8443",
        "path=/a/b",
        "search=?x=1",
        "hash=#f",
        "win=true",
        "tags=1,2",
    ]);
}
