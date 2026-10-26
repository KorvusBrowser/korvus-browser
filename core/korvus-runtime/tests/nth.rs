//! :nth-child(an+b/odd/even/N) + :nth-last-child.
use engine::Engine;
#[tokio::test]
async fn nth_child() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r##"<!doctype html><html><body><ul>
        <li>1</li><li>2</li><li>3</li><li>4</li><li>5</li><li>6</li></ul></body></html>"##).unwrap();
    e.eval("n", r##"
        print("n2=" + document.querySelector("li:nth-child(2)").textContent);
        print("odd=" + Array.from(document.querySelectorAll("li:nth-child(odd)")).map(x=>x.textContent).join(","));
        print("even=" + Array.from(document.querySelectorAll("li:nth-child(even)")).map(x=>x.textContent).join(","));
        print("3n=" + Array.from(document.querySelectorAll("li:nth-child(3n)")).map(x=>x.textContent).join(","));
        print("3n1=" + Array.from(document.querySelectorAll("li:nth-child(3n+1)")).map(x=>x.textContent).join(","));
        print("last2=" + document.querySelector("li:nth-last-child(2)").textContent);
    "##.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "n2=2",
        "odd=1,3,5",
        "even=2,4,6",
        "3n=3,6",
        "3n1=1,4",
        "last2=5",
    ]);
}
