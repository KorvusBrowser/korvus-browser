//! LIVE one-table gate for `HTMLImageElement` (Phase 3 Track C).
//!
//! Boots the real engine -- whose context creation now builds `HTMLImageElement` from its `interface!`
//! table (`install::live::install_window_html_image_element`): the 27-member prototype (native
//! CSSOM/loader ops + the reflect DSL over content attributes) spliced onto the real HTMLElement chain,
//! the `img` tag re-registered onto it, and the legacy `Image` factory sharing the prototype -- and
//! asserts the RUNNING surface is:
//!   * BYTE-identical: prototype member names/order/descriptor shape == the captured Brave surface, the
//!     full `HTMLElement -> Element -> Node -> EventTarget` chain, `[object HTMLImageElement]`, a
//!     non-newable interface object, `Image.prototype === HTMLImageElement.prototype`.
//!   * VALUE-identical: a set/get round-trip for a sampled member of every reflect kind (str/url/bool/
//!     ulong/enum) AND the native members return real values, not 0/"" stubs -- the HARD LESSON gate
//!     from Track B, proving the table reproduces the old `08b`/`08k`/`generated_reflect` layer before
//!     it is deleted.

use engine::Engine;

fn artifact(role: &str) -> String {
    std::fs::read_to_string(persona::shipped_artifact(role)).unwrap()
}

/// The ordered `name:kind:...` describe lines the installer must produce for HTMLImageElement's own
/// prototype, from the surface fixture, plus the trailing non-enumerable `constructor`.
fn expected_proto(iface: &str) -> Vec<String> {
    let fx: serde_json::Value = serde_json::from_str(&artifact("surface")).unwrap();
    let mut out: Vec<String> = fx["interfaces"][iface]["members"].as_array().unwrap().iter().map(|m| {
        let n = m["name"].as_str().unwrap();
        match m["kind"].as_str().unwrap() {
            "accessor" => format!("{n}:accessor:e=1:c=1:s={}", m["set"].as_bool().unwrap_or(false) as u8),
            "method" => format!("{n}:data:e=1:c=1:w=1"),
            k => panic!("unexpected kind {k}"),
        }
    }).collect();
    // `constructor` AT THE CAPTURE'S SEAT, not appended. It is the last member
    // installed at interface-definition time, so a member a PARTIAL interface adds lands AFTER it --
    // `Navigator` 32 of 66, `Document` 240 of 247, `Screen` 9 of 11 with `onchange` and `isExtended`
    // behind it. Appending it was right on 83 % of the surface and wrong on exactly the interfaces a
    // fingerprinting script reads, and the index has been in the bundle since `3a5f0b60`.
    let at = fx["interfaces"][iface]["ctorIndex"].as_u64().map(|n| n as usize).unwrap_or(out.len()).min(out.len());
    out.insert(at, "constructor:data:e=0:c=1:w=1".to_string());
    out
}

async fn page() -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e
}

/// BYTE-IDENTICAL: the running `HTMLImageElement.prototype` is the capture's 27-member surface, member
/// for member, in order, with the WebIDL descriptor shape -- built entirely by the table + installer.
#[tokio::test]
async fn live_html_image_element_prototype_is_byte_identical_to_the_capture() {
    let mut e = page().await;
    let describe = r#"const desc = (o) => Object.getOwnPropertyNames(o).map(n => {
        const d = Object.getOwnPropertyDescriptor(o, n);
        return (d.get || d.set)
            ? `${n}:accessor:e=${+d.enumerable}:c=${+d.configurable}:s=${+!!d.set}`
            : `${n}:data:e=${+d.enumerable}:c=${+d.configurable}:w=${+d.writable}`;
    }).join("\n");"#;
    let src = format!(r#"(() => {{ {describe} return desc(HTMLImageElement.prototype); }})()"#);
    let got = e.eval_value("img-desc", src).unwrap().as_str().unwrap().to_string();
    assert_eq!(
        got.lines().collect::<Vec<_>>(),
        expected_proto("HTMLImageElement"),
        "the running HTMLImageElement.prototype != the captured Brave surface (table/installer must reproduce it)",
    );
}

/// The rest of the surface the shape gate cannot reach from a bare runtime: the DEEP inheritance chain
/// (HTMLElement -> Element -> Node -> EventTarget), the `[object HTMLImageElement]` tag on a real
/// element, the non-newable interface object, and the `Image` legacy factory sharing the prototype so
/// `new Image()`/`createElement("img")` land on the table-built prototype.
///
/// `Image.length` ONCE EXPECTED 2, AND 2 WAS THIS ENGINE'S OWN MISTAKE. `html_image.rs`
/// passed the literal `2` to `build_ctor` (since deleted), this gate recorded what the engine did, and nothing else
/// looked. The capture records `interfaces.Image.length` = **0**, and headless Chrome
/// 150.0.7871.46 on this machine answers `Image.length` 0 (`Audio` 0, `Option` 0 with it; probed
/// 2026-09-12 over a loopback server). The factory reads the number off its generated row now
/// (`installer::row_ctor_length`), so the expectation here is the capture's rather than the call
/// site's.
#[tokio::test]
async fn live_html_image_element_chain_tag_and_image_factory() {
    let mut e = page().await;
    let src = r#"(() => {
        const P = HTMLImageElement.prototype;
        const img = document.createElement("img");
        let threw = "";
        try { new HTMLImageElement(); threw = "no-throw"; } catch (e) { threw = e.constructor.name + ":" + e.message; }
        return [
            Object.getPrototypeOf(P) === HTMLElement.prototype,                    // true
            Object.getPrototypeOf(HTMLElement.prototype) === Element.prototype,    // true
            Object.getPrototypeOf(Element.prototype) === Node.prototype,           // true
            Object.getPrototypeOf(Node.prototype) === EventTarget.prototype,       // true
            Object.prototype.toString.call(img),                                   // [object HTMLImageElement]
            threw,                                                                 // TypeError:Illegal constructor
            HTMLImageElement.length,                                               // 0
            Object.getPrototypeOf(img) === P,                                      // true (tag registered)
            img instanceof HTMLImageElement,                                       // true
            new Image() instanceof HTMLImageElement,                               // true
            Image.prototype === HTMLImageElement.prototype,                        // true
            Image.length,                                                          // 0 (the ROW's; Chrome 150 answers 0)
            new Image(3, 4).width + "x" + new Image(3, 4).height,                  // 3x4
            typeof Object.getOwnPropertyDescriptor(P, "width").get,                // function
        ].join("|");
    })()"#;
    let got = e.eval_value("img-chain", src.to_string()).unwrap().as_str().unwrap().to_string();
    assert_eq!(
        got,
        // `new X()` carries Chrome's `Failed to construct 'X': ` construction context.
        "true|true|true|true|[object HTMLImageElement]|TypeError:Failed to construct 'HTMLImageElement': Illegal constructor|0|true|true|true|true|0|3x4|function",
        "HTMLImageElement chain / tag / Image factory != expected",
    );
}

/// VALUE-IDENTICAL: a set/get round-trip for a sampled member of every reflect kind, and the native
/// members returning real values (not 0/"" stubs). This is the HARD LESSON gate: it proves the reflect
/// DSL + native ops reproduce the old layer's BEHAVIOR, not just its shape.
#[tokio::test]
async fn live_html_image_element_member_values_round_trip() {
    let mut e = page().await;
    let src = r#"(() => {
        const img = document.createElement("img");
        const out = [];
        const chk = (label, got, want) => out.push(label + "=" + (got === want ? "ok" : JSON.stringify(got)));

        // reflect str (alt): getAttribute||"" and setAttribute(String(v)).
        img.alt = "hello";               chk("alt.set", img.getAttribute("alt"), "hello");
        img.setAttribute("alt", "world"); chk("alt.get", img.alt, "world");
        chk("srcset.default", img.srcset, "");

        // reflect url (src): absolute URL round-trips; setter is the plain string setter.
        img.src = "https://ex.com/a.png"; chk("src.attr", img.getAttribute("src"), "https://ex.com/a.png");
        chk("src.get", img.src, "https://ex.com/a.png");

        // reflect bool (isMap): hasAttribute / toggle.
        chk("isMap.default", img.isMap, false);
        img.isMap = true;  chk("isMap.set", img.hasAttribute("ismap"), true);
        chk("isMap.get", img.isMap, true);
        img.isMap = false; chk("isMap.clear", img.hasAttribute("ismap"), false);

        // reflect ulong (hspace): default 0, parse, invalid -> 0.
        chk("hspace.default", img.hspace, 0);
        img.hspace = 5; chk("hspace.set", img.getAttribute("hspace"), "5");
        img.setAttribute("hspace", "7"); chk("hspace.get", img.hspace, 7);
        img.setAttribute("hspace", "nope"); chk("hspace.invalid", img.hspace, 0);

        // reflect enum (decoding: missing/invalid -> auto) and (referrerPolicy: missing -> "").
        chk("decoding.default", img.decoding, "auto");
        img.setAttribute("decoding", "sync"); chk("decoding.limit", img.decoding, "sync");
        img.setAttribute("decoding", "ZZZ");  chk("decoding.invalid", img.decoding, "auto");
        img.decoding = "async"; chk("decoding.setraw", img.getAttribute("decoding"), "async");
        chk("referrerPolicy.default", img.referrerPolicy, "");

        // native writable (width): default 0, parse attr, setter writes decimal.
        chk("width.default", img.width, 0);
        img.setAttribute("width", "42"); chk("width.attr", img.width, 42);
        img.width = 100; chk("width.set", img.getAttribute("width"), "100");

        // native readonly: real values, not missing/undefined. `complete` is FALSE here: a `src` was
        // assigned above and that request is in flight, so (stock Chrome 150, measured) `complete`
        // reads false from the synchronous point the src is set until the request settles.
        chk("complete", img.complete, false);
        chk("x", img.x, 0);
        chk("y", img.y, 0);
        chk("naturalWidth", img.naturalWidth, 0);
        chk("naturalHeight", img.naturalHeight, 0);
        chk("currentSrc", img.currentSrc, "");

        // native crossOrigin (limited-value reflection).
        img.setAttribute("crossorigin", "use-credentials"); chk("crossOrigin.get", img.crossOrigin, "use-credentials");
        img.crossOrigin = "anonymous"; chk("crossOrigin.set", img.getAttribute("crossorigin"), "anonymous");

        return out.join("\n");
    })()"#;
    let got = e.eval_value("img-values", src.to_string()).unwrap().as_str().unwrap().to_string();
    let failures: Vec<&str> = got.lines().filter(|l| !l.ends_with("=ok")).collect();
    assert!(failures.is_empty(), "HTMLImageElement member value round-trips failed:\n{}", failures.join("\n"));
}

/// `img.decode()` is the real native method, not a stub: it returns a Promise that REJECTS with an
/// `EncodingError` DOMException for an `<img>` with no decodable image (Brave 151's behavior).
#[tokio::test]
async fn live_html_image_element_decode_rejects() {
    let mut e = page().await;
    let src = r#"(() => {
        const img = document.createElement("img");
        return img.decode().then(() => "resolved", (err) => err.constructor.name + ":" + err.name);
    })()"#;
    let got = e
        .eval_value_settled("img-decode", src.to_string(), std::time::Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(
        got.as_str().unwrap(),
        "DOMException:EncodingError",
        "img.decode() must reject with an EncodingError DOMException (the real native body, not a stub)",
    );
}

/// A `srcset` image is read under the candidate it selected, and sized by that candidate's density: Chrome 150 on a
/// 20x10 PNG as `512w` for a 100px slot reads naturalWidth 3 and lays out 3.91 wide, as `2x` 10, and an `x` candidate
/// beats `src` (`currentSrc` the candidate's). Every `srcset` image here read 0x0 and painted nothing.
#[tokio::test]
async fn a_srcset_image_is_its_selected_candidate() {
    let script = r#"new Promise(res => {
        const png = "data:image/png;base64,PNG_B64";
        document.body.innerHTML = `<img id=a srcset="${png} 512w" sizes="100px"><img id=b srcset="${png} 2x"><img id=c src="${png}#s" srcset="${png}#x 1x">`;
        setTimeout(() => res([...document.images].map(i => [i.id, i.naturalWidth, i.naturalHeight, Math.round(i.getBoundingClientRect().width * 100) / 100, i.currentSrc.slice(-2)])), 500);
    })"#.replace("PNG_B64", "iVBORw0KGgoAAAANSUhEUgAAABQAAAAKCAIAAAA7N+mxAAAAF0lEQVR4nGM8ISfHQC5gIlvnqOYRoxkAhH4BGBokPGwAAAAASUVORK5CYII=");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, &script).await;
    assert_eq!(serde_json::Value::Array(got).to_string(), r##"[["a",3,1,3.91,"I="],["b",10,5,10,"I="],["c",20,10,20,"#x"]]"##);
}
