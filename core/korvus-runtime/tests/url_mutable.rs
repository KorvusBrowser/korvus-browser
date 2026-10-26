//! Native URL: setters (protocol/host/port/pathname/search/hash/username/password), origin,
//! base resolution, statics, searchParams write-back. Getters stay [native code] (see nolies).
use engine::Engine;
#[tokio::test]
async fn url_mutable() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.eval("a", r#"
        const u = new URL("https://user:pass@example.com:8080/path?a=b#frag");
        print("parse=" + [u.protocol, u.hostname, u.port, u.pathname, u.search, u.hash, u.username, u.password].join("|"));
        print("origin=" + u.origin);
        print("host=" + u.host + " href0=" + u.href);
        // base resolution
        const rel = new URL("/x/y?z=1", "https://base.example/old");
        print("base=" + rel.href);
        // setters
        u.protocol = "http:"; u.hostname = "other.com"; u.port = "9000";
        u.pathname = "/newpath"; u.search = "q=1"; u.hash = "top";
        print("mutated=" + u.href);
        u.port = ""; print("noport=" + u.host);
        // toString / statics
        print("toString=" + String(new URL("https://z.com/")));
        print("canParse=" + URL.canParse("https://ok.com") + "," + URL.canParse("not a url"));
        print("parseStatic=" + (URL.parse("bad url") === null) + "," + (URL.parse("https://ok.com/") instanceof URL));
        // searchParams write-back
        const s = new URL("http://p.com/");
        s.searchParams.append("k", "v");
        s.searchParams.append("k2", "v2");
        print("spSync=" + s.search + " href=" + s.href);
        // empty materialization must not add '?'
        const e2 = new URL("http://p.com/"); e2.searchParams; print("noQ=" + e2.href);
        print("createObjectURL=" + URL.createObjectURL(new Blob(["x"])).startsWith("blob:"));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "parse=https:|example.com|8080|/path|?a=b|#frag|user|pass",
        "origin=https://example.com:8080",
        "host=example.com:8080 href0=https://user:pass@example.com:8080/path?a=b#frag",
        "base=https://base.example/x/y?z=1",
        "mutated=http://user:pass@other.com:9000/newpath?q=1#top",
        "noport=other.com",
        "toString=https://z.com/",
        "canParse=true,false",
        "parseStatic=true,true",
        "spSync=?k=v&k2=v2 href=http://p.com/?k=v&k2=v2",
        "noQ=http://p.com/",
        "createObjectURL=true",
    ]);
}
