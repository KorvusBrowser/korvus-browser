//! createComment + replaceChild + append/prepend.
use engine::Engine;
#[tokio::test]
async fn dom_manipulation() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><ul id="l"><li>b</li></ul></body></html>"#).unwrap();
    e.eval("m", r#"
        const ul = document.getElementById("l");
        const a = document.createElement("li"); a.append("a");
        const c = document.createElement("li"); c.append("c");
        ul.prepend(a);
        ul.append(c);
        print("order=" + Array.from(ul.children).map(x => x.textContent).join(","));
        const z = document.createElement("li"); z.append("z");
        ul.replaceChild(z, ul.firstElementChild);
        print("replaced=" + Array.from(ul.children).map(x => x.textContent).join(","));
        const cm = document.createComment("hi");
        print("comment=" + cm.nodeName + "," + cm.nodeValue);
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "order=a,b,c",
        "replaced=z,b,c",
        "comment=#comment,hi",
    ]);
}

/// `replaceChild(fragment, old)` must REPLACE `old` with the FRAGMENT'S CHILDREN (DOM §4.2.3
/// "replace", node a DocumentFragment), not plant the fragment node. Regression (creepjs): `patch()`
/// does `parent.replaceChild(document.importNode(template.content, true), old)`; replaceChild was the
/// one insertion path that did not flatten a fragment, so the content landed detached-but-isConnected
/// with zero layout and the page body went empty.
#[tokio::test]
async fn replace_child_unwraps_document_fragment() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><div id="p"><span id="old">x</span></div></body></html>"#).unwrap();
    e.eval("m", r#"
        const p = document.getElementById("p");
        const old = document.getElementById("old");
        const t = document.createElement("template");
        t.innerHTML = '<b id="a">A</b><i id="c">C</i>';
        const frag = document.importNode(t.content, true);
        p.replaceChild(frag, old);
        print("children=" + p.childElementCount);
        print("html=" + p.innerHTML);
        print("fragEmpty=" + (frag.childElementCount === 0));
        print("aParent=" + (document.getElementById("a").parentElement === p));
        print("oldGone=" + (document.getElementById("old") === null));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "children=2",
        "html=<b id=\"a\">A</b><i id=\"c\">C</i>",
        "fragEmpty=true",
        "aParent=true",
        "oldGone=true",
    ]);
}

/// A frame appended NESTED inside other markup must still install `window[i]` (indexed access to the
/// child browsing context), not only bump `window.length` (a live walk). Regression (creepjs's
/// getPhantomIframe): `div.innerHTML='<iframe>'; body.appendChild(div)` left `window[len]` undefined
/// while `length` was 1, so its phantom-iframe probe saw `undefined` and reported every API corrupted.
#[tokio::test]
async fn window_indexed_access_for_nested_frame() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body></body></html>"#).unwrap();
    e.eval("m", r#"
        const before = window.length;
        const div = document.createElement("div");
        div.innerHTML = '<div><iframe></iframe></div>';
        document.body.appendChild(div);
        print("len=" + window.length);
        const w = window[before];
        print("indexedType=" + (w === undefined ? "undefined" : typeof w));
        const cw = document.querySelector("iframe").contentWindow;
        print("equalsContentWindow=" + (w === cw));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "len=1",
        "indexedType=object",
        "equalsContentWindow=true",
    ]);
}

/// `OfflineAudioContext.startRendering()` must fire a `complete` event a TASK later that reaches a
/// listener added with `addEventListener('complete')` AFTER the call -- creepjs's getRenderedBuffer
/// awaits exactly this, with no timeout. Regression: only the `oncomplete` attribute fired, and
/// synchronously, so the listener attached on the next line never heard it and the fingerprint hung.
#[tokio::test]
async fn offline_audio_context_fires_complete_to_add_event_listener() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body></body></html>"#).unwrap();
    e.eval("m", r#"
        const c = new OfflineAudioContext(1, 44100, 44100);
        c.addEventListener("complete", (ev) => {
            print("ael=" + (ev.renderedBuffer ? ev.renderedBuffer.length : "none"));
        });
        c.startRendering();
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec!["ael=44100"]);
}

/// `instanceof` on a prototype-less function (arrow / native method / accessor) throws a TypeError
/// whose 2nd stack line must read the bare `at [Symbol.hasInstance] (<anonymous>)` -- what stock
/// Chrome/Brave 151 emits. The vendored v8 crate (150) reports an empty function name for the
/// `Function.prototype[Symbol.hasInstance]` builtin, so deno_core's frame formatter used to prepend
/// the receiver's own name (`arrow.[Symbol.hasInstance]` / `get ua.[Symbol.hasInstance]`), which made
/// creepjs's proto lie-detector flag all 196 API properties as "corrupted" (its FUNCTION_INSTANCE
/// regex `/at (Function\.)?\[Symbol.hasInstance\]/` is anchored to that frame). Fixed in
/// vendor/deno_core/error.rs (format_frame: suppress the `Type.` prefix for non-identifier names,
/// matching V8's String::IsIdentifier guard).
#[tokio::test]
async fn instanceof_type_error_stack_frame_matches_real_browser() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body></body></html>"#).unwrap();
    e.eval("m", r#"
        const FUNCTION_INSTANCE = /^\s*at (Function\.)?\[Symbol.hasInstance\]/;
        function line1(fn){ try { fn instanceof fn; return "NOTHROW"; } catch(e){ return e.stack.split("\n")[1]; } }
        const arrow = () => {};
        const nativeMethod = Node.prototype.appendChild;
        const o = { get ua(){ return 1; } };
        const getter = Object.getOwnPropertyDescriptor(o, "ua").get;
        for (const [name, fn] of [["arrow", arrow], ["native", nativeMethod], ["getter", getter]]) {
            print(name + "=" + FUNCTION_INSTANCE.test(line1(fn)));
        }
        print("exact=" + JSON.stringify(line1(arrow).trim()));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "arrow=true",
        "native=true",
        "getter=true",
        "exact=\"at [Symbol.hasInstance] (<anonymous>)\"",
    ]);
}
