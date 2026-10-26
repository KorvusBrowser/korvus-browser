//! Korvus GS 2-A3: Shadow-DOM webapi surface end-to-end from page JS.
//! Exercises element.attachShadow / element.shadowRoot / ShadowRoot (host/mode/querySelector[All]/
//! getElementById/innerHTML) plus the shadow-aware Node.isConnected / getRootNode, and proves the
//! encapsulation boundary (document.querySelector must not pierce a shadow tree).
use engine::Engine;
#[tokio::test]
async fn shadow_dom_webapi_surface_is_usable_from_js() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(
        r#"<!doctype html><html><body>
             <div id="host"></div>
             <div id="host2"></div>
           </body></html>"#,
    )
    .unwrap();
    e.eval(
        "shadow",
        r##"
        try {
          const host = document.getElementById("host");

          // (1) attachShadow(open) -> a ShadowRoot (its own interface, not an Element).
          const sr = host.attachShadow({ mode: "open" });
          print("isSR=" + (sr instanceof ShadowRoot));
          print("notEl=" + !(sr instanceof Element));
          print("mode=" + sr.mode);
          print("hostEq=" + (sr.host === host));

          // (2) set shadow content; shadow-scoped queries find it.
          sr.innerHTML = "<div class='x' id='inner'>shadow!</div><span class='y'>s</span>";
          const inner = sr.querySelector(".x");
          print("qsFound=" + (inner ? inner.textContent : "NULL"));
          print("qsaCount=" + sr.querySelectorAll("div,span").length);
          print("gebiEq=" + (sr.getElementById("inner") === inner));
          print("innerHtml=" + sr.innerHTML);

          // (3) the encapsulation boundary: document.* must NOT pierce the shadow tree.
          print("docQS=" + (document.querySelector(".x") === null));
          print("docAll=" + document.querySelectorAll(".x").length);
          print("docGebi=" + (document.getElementById("inner") === null));

          // (4) el.shadowRoot returns the OPEN root (same object).
          print("srEq=" + (host.shadowRoot === sr));

          // (5) a CLOSED shadow root is hidden from el.shadowRoot.
          const host2 = document.getElementById("host2");
          const sr2 = host2.attachShadow({ mode: "closed" });
          print("closedMode=" + sr2.mode);
          print("closedHidden=" + (host2.shadowRoot === null));

          // (6) isConnected reflects host connectivity across the shadow edge.
          print("innerConn=" + inner.isConnected);
          const detached = document.createElement("div");
          const dsr = detached.attachShadow({ mode: "open" });
          dsr.innerHTML = "<b class='z'>z</b>";
          print("detachedConn=" + dsr.querySelector(".z").isConnected);

          // (7) getRootNode(): shadow root for shadow content; composed reaches the document.
          print("rootIsSR=" + (inner.getRootNode() === sr));
          print("composedDoc=" + (inner.getRootNode({ composed: true }).nodeType === 9));

          // (8) a second attachShadow throws NotSupportedError.
          let threw = "no";
          try { host.attachShadow({ mode: "open" }); } catch (err) { threw = err.name; }
          print("reattach=" + threw);
        } catch (e) {
          print("ERR=" + (e && e.stack ? e.stack : e));
        }
        "##
        .into(),
    )
    .unwrap();
    e.run().await.unwrap();
    assert_eq!(
        e.take_output(),
        vec![
            "isSR=true",
            "notEl=true",
            "mode=open",
            "hostEq=true",
            "qsFound=shadow!",
            "qsaCount=2",
            "gebiEq=true",
            "innerHtml=<div class=\"x\" id=\"inner\">shadow!</div><span class=\"y\">s</span>",
            "docQS=true",
            "docAll=0",
            "docGebi=true",
            "srEq=true",
            "closedMode=closed",
            "closedHidden=true",
            "innerConn=true",
            "detachedConn=false",
            "rootIsSR=true",
            "composedDoc=true",
            "reattach=NotSupportedError",
        ]
    );
}
