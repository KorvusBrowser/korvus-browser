//! createTextNode + insertBefore.
use engine::Engine;
#[tokio::test]
async fn create_text_and_insert() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><ul id="l"><li>b</li><li>c</li></ul></body></html>"#).unwrap();
    e.eval("i", r#"
        const ul = document.getElementById("l");
        const li = document.createElement("li");
        li.appendChild(document.createTextNode("a"));
        ul.insertBefore(li, ul.firstElementChild);
        print("order=" + Array.from(ul.children).map(c => c.textContent).join(","));
        print("count=" + ul.childElementCount);
        print("inner=" + ul.innerHTML);
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "order=a,b,c",
        "count=3",
        "inner=<li>a</li><li>b</li><li>c</li>",
    ]);
}

/// Inserting a `<script>` into the document RUNS it -- the algorithm moved out of
/// `KB.__runInsertedScript` and into `core/html/html_script_element.rs`.
///
/// Every branch of the type test is here, because each is a case the JS body got right and a rewrite
/// could silently lose: `type=""` runs and `type=" "` does not (the emptiness check is on the RAW
/// value, before stripping); `type="javascript"` is not a MIME type and does not run;
/// `type="application/ecmascript"` is and does; `language` is NOT whitespace-stripped; a `src` is a
/// fetch this path cannot perform and so runs nothing; a script runs at most ONCE however often it is
/// re-inserted; and `document.currentScript` is the running script while it runs and null after.
#[tokio::test]
async fn inserting_a_script_runs_it() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body></body></html>"#).unwrap();
    e.eval("i", r#"
        const add = (attrs, code) => {
            const s = document.createElement("script");
            for (const k in attrs) s.setAttribute(k, attrs[k]);
            s.textContent = code;
            document.body.appendChild(s);
            return s;
        };
        globalThis.ran = [];
        add({}, "globalThis.ran.push('plain')");
        add({ type: "" }, "globalThis.ran.push('empty-type')");
        add({ type: " " }, "globalThis.ran.push('space-type')");
        add({ type: "javascript" }, "globalThis.ran.push('bare-word')");
        add({ type: "application/ecmascript" }, "globalThis.ran.push('ecmascript')");
        add({ type: "TEXT/JavaScript" }, "globalThis.ran.push('cased')");
        add({ language: "javascript" }, "globalThis.ran.push('language')");
        add({ language: " javascript" }, "globalThis.ran.push('language-space')");
        add({ src: "nope.js" }, "globalThis.ran.push('had-src')");
        print("ran=" + globalThis.ran.join(","));
        // A `var` declaration lands on the global: the body is a TOP-LEVEL script, not a function.
        add({}, "var korvusTopLevel = 7");
        print("global=" + globalThis.korvusTopLevel);
        // Once, however often it is re-inserted.
        const once = add({}, "globalThis.ran.push('once')");
        document.body.removeChild(once);
        document.body.appendChild(once);
        print("once=" + globalThis.ran.filter(x => x === "once").length);
        // currentScript is the running script, and null outside it.
        add({}, "globalThis.cur = document.currentScript && document.currentScript.tagName");
        print("cur=" + globalThis.cur + " after=" + document.currentScript);
        // `text` is the child TEXT content, and assigning it replaces every child.
        const t = document.createElement("script");
        t.appendChild(document.createTextNode("a"));
        t.appendChild(document.createComment("c"));
        print("text=" + t.text);
        t.text = "zz";
        print("afterSet=" + t.childNodes.length + ":" + t.text);
        // `async` is force-async on a created element and the attribute on a parsed one.
        print("async=" + document.createElement("script").async);
        print("supports=" + HTMLScriptElement.supports("classic") + "," + HTMLScriptElement.supports("nope"));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "ran=plain,empty-type,ecmascript,cased,language",
        "global=7",
        "once=1",
        "cur=SCRIPT after=null",
        "text=a",
        "afterSet=1:zz",
        "async=true",
        "supports=true,false",
    ]);
}

/// `document.currentScript` NESTS, and a script that inserts a script must restore the outer one.
///
/// THE CASE THE SIMPLE TEST ABOVE CANNOT SEE, and the reason `run_inserted_script` saves and restores
/// rather than setting and clearing. It is also the case that had to be kept when moving the
/// store off the `KB.__curScript` bridge slot and onto a V8 Private of the global: a save/restore
/// written against one holder and a read written against another agree on every non-nested page, so
/// the simple assertion is exactly the one that would not have caught the mistake.
///
/// THERE ARE TWO WRITERS AND THIS EXERCISES BOTH. `korvus-runtime/src/script_exec.rs` sets it around
/// each script the page driver runs (`load_page`, which is why this is not `load_html`), and
/// `core/html/html_script_element.rs` saves and restores around a dynamically inserted one. The last
/// line is the one that joins them: when the OUTER inserted script returns, `currentScript` must go
/// back to the PARSED script that is still running -- not to null, which is what a set-and-clear gives.
///
/// Chrome 151 headless, measured 2026-09-12 on this exact page:
///     parsed=SCRIPT# | in-outer=SCRIPT#outer | in-inner=SCRIPT#inner
///                    | after-inner=SCRIPT#outer | after-outer=SCRIPT#
#[tokio::test]
async fn nested_current_script_restores_the_outer_script() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_page(
        r##"<!doctype html><html><body><script>
        window.log = [];
        window.tag = function (x) { return x ? x.tagName + "#" + (x.id || "") : String(x); };
        window.add = function (id, code) {
            var s = document.createElement("script"); s.id = id; s.textContent = code;
            document.body.appendChild(s); return s;
        };
        log.push("parsed=" + tag(document.currentScript));
        add("outer",
          "window.log.push('in-outer=' + window.tag(document.currentScript));" +
          "window.add('inner', \"window.log.push('in-inner=' + window.tag(document.currentScript));\");" +
          "window.log.push('after-inner=' + window.tag(document.currentScript));");
        log.push("after-outer=" + tag(document.currentScript));
        </script></body></html>"##,
        "http://127.0.0.1:8732/nest.html",
        &[],
    )
    .await
    .expect("load_page");
    e.run().await.unwrap();
    e.eval("n", r#"print(window.log.join(" | "));"#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(
        e.take_output(),
        vec!["parsed=SCRIPT# | in-outer=SCRIPT#outer | in-inner=SCRIPT#inner | after-inner=SCRIPT#outer | after-outer=SCRIPT#"],
    );
}

/// The store `document.currentScript` reads is NOT REACHABLE FROM THE PAGE.
///
/// It was `globalThis[Symbol.for("korvus.bridge")].__curScript` -- an ordinary property of an ordinary
/// object, so a page could read which script the engine thought was running, write a different answer,
/// or freeze the holder and silently break the restore above. A V8 Private is invisible to every
/// reflection path, so the only way to the value is the spec'd getter.
#[tokio::test]
async fn the_current_script_store_is_not_page_visible() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body></body></html>"#).unwrap();
    e.eval("p", r#"
        print("holder=" + typeof globalThis[Symbol.for("korvus.bridge")]);
        print("named=" + Object.getOwnPropertyNames(globalThis).filter(k => /curScript/i.test(k)).length);
        // Writing the old name gets a plain global and changes no answer the getter gives.
        globalThis.__curScript = document.createElement("script");
        const s = document.createElement("script");
        s.textContent = "globalThis.seen = document.currentScript && document.currentScript.tagName";
        document.body.appendChild(s);
        print("seen=" + globalThis.seen + " after=" + document.currentScript);
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "holder=undefined",
        "named=0",
        "seen=SCRIPT after=null",
    ]);
}
