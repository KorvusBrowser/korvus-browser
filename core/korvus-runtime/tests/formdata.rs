//! FormData(form) scraping + form.elements + iteration.
use engine::Engine;
#[tokio::test]
async fn formdata_from_form() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body>
        <form id="f">
            <input name="user" value="alice">
            <input name="pw" value="secret" type="password">
            <input name="agree" type="checkbox" value="yes">
            <input name="news" type="checkbox" value="on" checked>
            <input name="ignore" type="submit" value="Go">
            <select name="role"><option value="admin" selected>Admin</option></select>
        </form>
    </body></html>"#).unwrap();
    e.eval("a", r#"
        const f = document.getElementById("f");
        const fd = new FormData(f);
        print("user=" + fd.get("user"));
        print("pw=" + fd.get("pw"));
        print("agree=" + fd.has("agree"));
        print("news=" + fd.get("news"));
        print("entries=" + [...fd.entries()].map(([k,v]) => k+"="+v).join("&"));
        fd.append("extra", "1");
        print("keys=" + [...fd.keys()].join(","));
        print("elements=" + f.elements.length + ",named=" + (f.elements.namedItem("user").value));
    "#.into()).unwrap();
    e.run().await.unwrap();
    let out = e.take_output();
    assert_eq!(out[0], "user=alice");
    assert_eq!(out[1], "pw=secret");
    assert_eq!(out[2], "agree=false");
    assert_eq!(out[3], "news=on");
    assert_eq!(out[5], "keys=user,pw,news,role,extra");
    assert_eq!(out[6].split(",named=").nth(1).unwrap(), "alice");
}
