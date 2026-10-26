//! Web Storage API: localStorage/sessionStorage getItem/setItem/removeItem/clear/length/key.
use engine::Engine;
#[tokio::test]
async fn web_storage() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.eval("s", r#"
        localStorage.setItem("a", "1");
        localStorage.setItem("b", "2");
        print("get=" + localStorage.getItem("a"));
        print("len=" + localStorage.length);
        print("miss=" + (localStorage.getItem("z") === null));
        localStorage.removeItem("a");
        print("afterRm=" + localStorage.length + "," + (localStorage.getItem("a") === null));
        // sessionStorage is a separate area
        sessionStorage.setItem("s", "x");
        print("sep=" + localStorage.getItem("s") + "," + sessionStorage.getItem("s"));
        localStorage.clear();
        print("cleared=" + localStorage.length);
        print("isStorage=" + (localStorage instanceof Storage));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "get=1", "len=2", "miss=true", "afterRm=1,true", "sep=null,x", "cleared=0", "isStorage=true",
    ]);
}
