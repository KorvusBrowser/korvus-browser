//! `getComputedStyle`, answered by the cascade.
//!
//! It used to resolve a page's inline `style` attribute over a hardcoded table of forty tag names,
//! guessing `block` for anything else, and returned `""` for every property nobody had inline-set.
//! Both are one-line detects: a browser always has an answer, and `<li>` is `list-item`.
use engine::Engine;
async fn run(html: &str, script: &str) -> Vec<String> {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(html).unwrap();
    e.eval("t", script.to_string()).unwrap();
    e.run().await.unwrap();
    e.take_output()
}

#[tokio::test]
async fn an_inline_style_computes_rather_than_echoing() {
    let out = run(
        r#"<!doctype html><html><body><div id="d" style="color: red; margin-top: 4px"></div></body></html>"#,
        r#"
        const cs = getComputedStyle(document.getElementById("d"));
        print("color=" + cs.color);
        print("margin=" + cs.getPropertyValue("margin-top"));
        print("camel=" + cs.marginTop);
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec!["color=rgb(255, 0, 0)", "margin=4px", "camel=4px"],
        "`red` is not a computed value; `rgb(255, 0, 0)` is"
    );
}

// The tags the forty-name table got wrong.
#[tokio::test]
async fn display_comes_from_the_user_agent_stylesheet() {
    let out = run(
        r#"<!doctype html><html><body><div id="d"></div><span id="s"></span><li id="l"></li>
           <table id="tb"><tr id="r"><td id="c"></td></tr></table><script id="sc"></script></body></html>"#,
        r#"
        for (const id of ["d", "s", "l", "tb", "r", "c", "sc"]) {
          print(id + "=" + getComputedStyle(document.getElementById(id)).display);
        }
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec!["d=block", "s=inline", "l=list-item", "tb=table", "r=table-row", "c=table-cell", "sc=none"]
    );
}

// A property nobody set has an answer, because the initial value is an answer.
#[tokio::test]
async fn every_property_has_a_value_even_when_nothing_set_it() {
    let out = run(
        r#"<!doctype html><html><body><p id="p"></p></body></html>"#,
        r#"
        const cs = getComputedStyle(document.getElementById("p"));
        print("fontSize=" + cs.fontSize);
        print("color=" + cs.color);
        print("visibility=" + cs.visibility);
        print("length=" + (cs.length > 100));
        print("item0=" + (cs.item(0).length > 0));
        print("notACssProperty=" + cs.getPropertyValue("banana") + ".");
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "fontSize=16px",
            "color=rgb(0, 0, 0)",
            "visibility=visible",
            "length=true",
            "item0=true",
            "notACssProperty=.",
        ]
    );
}

// The whole cascade, through the DOM: a <style> element nobody touched still styles the page.
#[tokio::test]
async fn a_style_element_styles_the_document_without_script_touching_it() {
    let out = run(
        r#"<!doctype html><html><head><style>
             p { color: red; font-size: 20px }
             .lede { color: blue }
             #target { margin-top: 2em }
           </style></head><body><p id="target" class="lede">x</p></body></html>"#,
        r#"
        const cs = getComputedStyle(document.getElementById("target"));
        print("color=" + cs.color);
        print("fontSize=" + cs.fontSize);
        print("marginTop=" + cs.marginTop);
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "color=rgb(0, 0, 255)",       // the class beats the type selector
            "fontSize=20px",
            "marginTop=40px",             // 2em of this element's own 20px
        ]
    );
}

#[tokio::test]
async fn an_inline_style_beats_the_sheet_and_important_beats_the_inline_style() {
    let out = run(
        r#"<!doctype html><html><head><style>
             #a { color: red } #b { color: red !important }
           </style></head><body>
           <p id="a" style="color: blue">x</p><p id="b" style="color: blue">y</p></body></html>"#,
        r#"
        print("a=" + getComputedStyle(document.getElementById("a")).color);
        print("b=" + getComputedStyle(document.getElementById("b")).color);
        "#,
    )
    .await;
    assert_eq!(out, vec!["a=rgb(0, 0, 255)", "b=rgb(255, 0, 0)"]);
}

#[tokio::test]
async fn inherited_properties_reach_down_the_tree() {
    let out = run(
        r#"<!doctype html><html><head><style>#outer { color: red; width: 500px }</style></head>
           <body><div id="outer"><span><em id="inner">x</em></span></div></body></html>"#,
        r#"
        const cs = getComputedStyle(document.getElementById("inner"));
        print("color=" + cs.color);
        print("width=" + cs.width);
        "#,
    )
    .await;
    assert_eq!(out, vec!["color=rgb(255, 0, 0)", "width=auto"], "colour inherits, width does not");
}

// The CSSOM and the cascade are the same sheets: an inserted rule has to take effect.
#[tokio::test]
async fn a_rule_inserted_through_the_cssom_changes_what_elements_compute_to() {
    let out = run(
        r#"<!doctype html><html><head><style id="s">p { color: red }</style></head>
           <body><p id="p">x</p></body></html>"#,
        r#"
        const p = document.getElementById("p");
        print("before=" + getComputedStyle(p).color);
        document.getElementById("s").sheet.insertRule("p { color: blue }", 1);
        print("after=" + getComputedStyle(p).color);
        "#,
    )
    .await;
    assert_eq!(out, vec!["before=rgb(255, 0, 0)", "after=rgb(0, 0, 255)"]);
}

// Appending a <style> is how a theme switch and every CSS-in-JS library work.
#[tokio::test]
async fn a_style_element_appended_by_script_takes_effect() {
    let out = run(
        r#"<!doctype html><html><body><p id="p">x</p></body></html>"#,
        r#"
        const p = document.getElementById("p");
        print("before=" + getComputedStyle(p).color);
        const s = document.createElement("style");
        s.textContent = "p { color: rebeccapurple }";
        document.head.appendChild(s);
        print("after=" + getComputedStyle(p).color);
        "#,
    )
    .await;
    assert_eq!(out, vec!["before=rgb(0, 0, 0)", "after=rgb(102, 51, 153)"]);
}

// A pseudo-element Chrome styles is a declaration of its own, inheriting from the element -- and one it
// does not know is the EMPTY declaration, not the originating element's style: a page checking
// `getComputedStyle(el, "::nonsense").length === 0` is asking whether the pseudo exists. Until
// P11c every pseudo-element answered empty here; Chrome 150 (this markup, over CDP) answers
// `::before`, `::highlight(name)` and the legacy `:before` with the element's full length and
// `rgb(0, 0, 0)`, and `::nonsense` with 0 and "".
#[tokio::test]
async fn a_pseudo_element_argument_gives_its_own_declaration_and_an_unknown_one_an_empty_one() {
    let out = run(
        r#"<!doctype html><html><body><li id="l">x</li></body></html>"#,
        r#"
        const li = document.getElementById("l");
        print("plain=" + (getComputedStyle(li).length > 0));
        for (const p of ["::before", "::nonsense", "::highlight(name)", ":before"]) {
          print(p + "=" + (getComputedStyle(li, p).length === getComputedStyle(li).length) + "/" + getComputedStyle(li, p).color + ".");
        }
        print("empty=" + (getComputedStyle(li, "").length > 0));
        print("null=" + (getComputedStyle(li, null).length > 0));
        // Without a leading colon it is not a pseudo-element request at all -- CSSOM says ignore it.
        print("noColon=" + (getComputedStyle(li, "before").length > 0));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "plain=true",
            "::before=true/rgb(0, 0, 0).",
            "::nonsense=false/.",
            "::highlight(name)=true/rgb(0, 0, 0).",
            ":before=true/rgb(0, 0, 0).",
            "empty=true",
            "null=true",
            "noColon=true",
        ]
    );
}

// Custom properties inherit, keep their exact case, and are enumerated alongside the rest.
#[tokio::test]
async fn custom_properties_are_readable_from_the_computed_style() {
    let out = run(
        r#"<!doctype html><html><head><style>#outer { --brand: #f00 }</style></head>
           <body><div id="outer"><p id="p" style="--local: 1px">x</p></div></body></html>"#,
        r#"
        const cs = getComputedStyle(document.getElementById("p"));
        print("inherited=" + cs.getPropertyValue("--brand"));
        print("inline=" + cs.getPropertyValue("--local"));
        print("absent=" + cs.getPropertyValue("--nope") + ".");
        const el = document.getElementById("p");
        el.style.setProperty("--long", "1111111111111111111111111");
        print("long=" + getComputedStyle(el).getPropertyValue("--long"));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "inherited=#f00",
            "inline=1px",
            "absent=.",
            // A long integer must not turn into scientific notation on the way through.
            "long=1111111111111111111111111",
        ]
    );
}

// The measurement that put var() in v1: 28.8% of the declarations in a corpus of real stylesheets
// are written this way, and without substitution `getComputedStyle(el).color` hands back the
// literal string `var(--brand)`.
#[tokio::test]
async fn var_references_resolve_through_the_cascade() {
    let out = run(
        r#"<!doctype html><html><head><style>
             :root { --brand: #f00; --gap: 4px }
             #target { color: var(--brand); margin: var(--gap); width: var(--missing, 7px) }
           </style></head><body><p id="target">x</p></body></html>"#,
        r#"
        const cs = getComputedStyle(document.getElementById("target"));
        print("color=" + cs.color);
        print("marginTop=" + cs.marginTop + " marginLeft=" + cs.marginLeft);
        print("width=" + cs.width);
        print("custom=" + cs.getPropertyValue("--brand"));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "color=rgb(255, 0, 0)",
            // A shorthand written with var() cannot be expanded until the value is known.
            "marginTop=4px marginLeft=4px",
            "width=7px",
            "custom=#f00",
        ]
    );
}

// ── The declaration resolves on access, and that is what Chrome does ────────────────
//
// `getComputedStyle` used to materialise all 475 properties into two V8 objects before it returned,
// so a caller that read one paid for 475 -- about 1,900 V8 operations -- and a caller that read none
// paid the same. On a synthetic 2,006-element page the call alone cost 355 ms here against 0.2 ms in
// Chrome 150. It now hands back a declaration carrying the ELEMENT and answers each member from the
// cascade when that member is asked.
//
// Making it lazy also made it LIVE, and live is what Chrome is. Measured over CDP on Chrome
// 150.0.7871.46, which is the answer this test pins:
//
//   const s = getComputedStyle(el); el.style.color = 'red'; s.color
//     chrome  rgb(0, 0, 255) -> rgb(255, 0, 0)      korvus before this change: stayed blue
//
// The brief for this lane expected the opposite -- that a snapshot had to be preserved and that a
// lazy implementation would have to earn it. The one-line experiment says Chrome's result is live,
// so the eager pack was the bug and the fast path is also the faithful one.
#[tokio::test]
async fn a_computed_style_follows_the_element_the_way_chromes_does() {
    let out = run(
        r#"<!doctype html><html><body><div id="d" style="color: blue">x</div></body></html>"#,
        r#"
        const el = document.getElementById("d");
        const s = getComputedStyle(el);
        print("before=" + s.color);
        el.style.color = "red";
        // The SAME object, read again. A snapshot would still say blue.
        print("afterMutation=" + s.color);
        print("viaGetPropertyValue=" + s.getPropertyValue("color"));

        // `length` is live too: a custom property is one more enumerated name.
        const lenBefore = s.length;
        el.style.setProperty("--x", "1px");
        print("lengthGrew=" + (s.length === lenBefore + 1));
        print("customReadsBack=" + s.getPropertyValue("--x"));

        // A style RULE changing, not just the inline attribute.
        const sheet = document.createElement("style");
        sheet.textContent = '#d { visibility: hidden }';
        document.head.appendChild(sheet);
        print("ruleSeen=" + s.visibility);

        // Chrome: a declaration taken while connected reports 0 after the element is removed, and
        // one taken while DETACHED comes alive when the element is appended. Both measured.
        const det = document.createElement("div");
        const sd = getComputedStyle(det);
        print("detached=" + sd.length + "/" + sd.display + ".");
        document.body.appendChild(det);
        print("thenAttached=" + (sd.length > 0) + "/" + sd.display);
        el.remove();
        print("afterRemove=" + s.length + "/" + s.color + ".");
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "before=rgb(0, 0, 255)",
            "afterMutation=rgb(255, 0, 0)",
            "viaGetPropertyValue=rgb(255, 0, 0)",
            "lengthGrew=true",
            "customReadsBack=1px",
            "ruleSeen=hidden",
            "detached=0/.",
            "thenAttached=true/block",
            "afterRemove=0/.",
        ]
    );
}

// Every observable the eager pack answered, answered the same way by the lazy one. The members here
// are exactly the ones a page reaches a computed style through, and each of them used to read out of
// the materialised pair: `length` counted the names array, `item` and `cs[i]` indexed it, a named
// read and `getPropertyValue` looked the dashed name up in the values bag. `Object.keys` was the names
// array verbatim; it is what Blink's interceptors list: the indices "0" ..
// length-1, then the build's named properties -- stock Chrome 152 lists 477 + 737 = 1214 for this page
// (measured 2026-10-16; Chrome 150 listed 475 + 735).
#[tokio::test]
async fn the_lazy_declaration_answers_what_the_eager_pack_answered() {
    let out = run(
        r#"<!doctype html><html><body><div id="d" style="padding-left: 3px; --brand: #f00">x</div></body></html>"#,
        r#"
        const el = document.getElementById("d");
        const s = getComputedStyle(el);
        const keys = Object.keys(s);
        // 476 enumerated properties, recorded from the worn persona's capture, plus this element's one
        // custom property.
        print("length=" + s.length);
        print("keys=" + keys.length);
        print("keysAreTheIndicesThenTheNames=" + (keys.slice(0, s.length).every((k, i) => k === String(i))
            && keys.slice(s.length).every((k) => !/^\d+$/.test(k) && typeof s[k] === "string")));
        print("first=" + s.item(0) + " " + s[0]);
        print("last=" + s.item(s.length - 1) + " " + s[s.length - 1]);
        // `item` past the end is "" and `s[i]` past the end is `undefined` -- not the same answer,
        // and both are Chrome's (measured).
        print("pastTheEnd=" + s.item(s.length) + "." + s[s.length] + ".");
        print("negative=" + s.item(-1) + ".");
        // A named read dashes the name; `getPropertyValue` takes the dashed one.
        print("named=" + s.paddingLeft + " " + s.getPropertyValue("padding-left"));
        print("unknown=" + s.getPropertyValue("banana") + "." + s.getPropertyValue("colr") + ".");
        print("custom=" + s.getPropertyValue("--brand"));
        // The interface identity the old Proxy lost.
        print("instanceof=" + (s instanceof CSSStyleDeclaration));
        print("brand=" + Object.prototype.toString.call(s));
        print("cssText=" + s.cssText + ".");
        // A computed block is read-only.
        let threw = "no";
        try { s.setProperty("color", "green"); } catch (e) { threw = e.name; }
        print("setProperty=" + threw);
        print("stillBlack=" + s.color);
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "length=477",
            "keys=1214",
            "keysAreTheIndicesThenTheNames=true",
            "first=accent-color accent-color",
            "last=--brand --brand",
            "pastTheEnd=.undefined.",
            "negative=.",
            "named=3px 3px",
            "unknown=..",
            "custom=#f00",
            "instanceof=true",
            "brand=[object CSSStyleDeclaration]",
            "cssText=.",
            "setProperty=NoModificationAllowedError",
            "stillBlack=rgb(0, 0, 0)",
        ]
    );
}

// ── The ancestor chain, through the real `getComputedStyle` ─────────────────────────
//
// P8b stopped the cascade re-walking the whole ancestor chain for every element: the walk now
// resumes from the nearest ancestor whose style this generation already banked. The COUNT that says
// so is `korvus-page`'s `reading_a_deeper_document_does_not_cost_more_cascades`; this is the other
// half -- that resuming mid-chain returns the same answers a walk from the root does.
//
// Every expectation below is stock Google Chrome 150.0.7871.46's, measured on 2026-09-14 on a
// loopback fixture (`--headless --dump-dom`), and this engine already agreed with it row for row
// BEFORE the change. That is the point: the rows exist so that it still does.
//
// Four of them are the ways the answer at the bottom of a chain moves without the bottom element
// being touched at all, which is precisely what a chain that resumes from above can get wrong:
//
//   * a mutation 100 links up             #d100's inline `color` reaches the leaf
//   * a sheet rule matching 100 links up  no element touched at all
//   * a NEARER ancestor re-declaring a custom property -- var() takes the closer one
//   * REPARENTING                         no style changed anywhere; the chain itself moved, and
//                                         the leaf's inherited `font-size` goes 31px -> 40px for it
#[tokio::test]
async fn a_two_hundred_deep_chain_answers_what_chromes_does() {
    let out = run(
        r#"<!doctype html><html><head><style>
             #top { color: rgb(1, 2, 3); font-size: 20px; letter-spacing: 3px; --brand: rgb(10, 20, 30) }
             #other { font-size: 40px; color: rgb(7, 7, 7); --brand: rgb(40, 50, 60) }
             .kw-inherit { color: inherit }
             .kw-initial { color: initial }
             .kw-unset   { color: unset }
             .kw-revert  { color: revert }
             .var        { color: var(--brand) }
           </style></head>
           <body><div id="top"><div id="mid"></div></div><div id="other"></div></body></html>"#,
        // `r##"` because the script writes a selector as `"#d100 ..."`, and `"#` ends an `r#"`
        // literal. The compiler said so; it is not a style choice.
        r##"
        let cur = document.getElementById("mid");
        for (let i = 0; i < 200; i++) {
          const d = document.createElement("div");
          d.id = "d" + i;
          cur.appendChild(d);
          cur = d;
        }
        const leaf = document.createElement("span");
        leaf.textContent = "x";
        cur.appendChild(leaf);

        // ONE declaration, read again after every mutation below -- P8a's liveness, over 200 links.
        const s = getComputedStyle(leaf);
        print("inheritedThrough200=" + s.color + " " + s.fontSize + " " + s.letterSpacing);

        // The four CSS-wide keywords, applied 100 links down and read at the bottom.
        const midway = document.getElementById("d100");
        for (const cls of ["kw-inherit", "kw-initial", "kw-unset", "kw-revert"]) {
          midway.className = cls;
          print(cls + "=" + getComputedStyle(midway).color + " leaf=" + s.color);
        }
        midway.className = "";
        print("cleared=" + s.color);

        // A mutation 100 links above the leaf, and one 101 links above it.
        midway.style.color = "rgb(9, 8, 7)";
        print("mutatedMidway=" + s.color);
        document.getElementById("top").style.fontSize = "31px";
        print("mutatedTop=" + s.fontSize);

        // A sheet rule that matches midway, with no element touched.
        const sh = document.createElement("style");
        sh.textContent = "#d100 { letter-spacing: 11px }";
        document.head.appendChild(sh);
        print("sheetMidway=" + s.letterSpacing);

        // var() resolved through the chain, then a NEARER ancestor re-declaring the property.
        leaf.className = "var";
        print("varFromTop=" + s.color + " brand=" + s.getPropertyValue("--brand"));
        midway.style.setProperty("--brand", "rgb(70, 80, 90)");
        print("varNearerWins=" + s.color + " brand=" + s.getPropertyValue("--brand"));

        // REPARENTING: no style changes anywhere, but what the leaf inherits does.
        document.getElementById("other").appendChild(document.getElementById("mid"));
        print("reparented=" + s.color + " " + s.fontSize + " brand=" + s.getPropertyValue("--brand"));
        midway.style.removeProperty("--brand");
        print("reparentedNoOverride=" + s.color + " brand=" + s.getPropertyValue("--brand"));
        "##,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "inheritedThrough200=rgb(1, 2, 3) 20px 3px",
            "kw-inherit=rgb(1, 2, 3) leaf=rgb(1, 2, 3)",
            "kw-initial=rgb(0, 0, 0) leaf=rgb(0, 0, 0)",
            "kw-unset=rgb(1, 2, 3) leaf=rgb(1, 2, 3)",
            // `revert` with no user stylesheet under it is `unset`, and Chrome agrees here.
            "kw-revert=rgb(1, 2, 3) leaf=rgb(1, 2, 3)",
            "cleared=rgb(1, 2, 3)",
            "mutatedMidway=rgb(9, 8, 7)",
            "mutatedTop=31px",
            "sheetMidway=11px",
            "varFromTop=rgb(10, 20, 30) brand=rgb(10, 20, 30)",
            "varNearerWins=rgb(70, 80, 90) brand=rgb(70, 80, 90)",
            // The subtree moved under a 40px ancestor: `font-size` follows the new chain, and
            // `color` does not, because #d100 still sets it on the way down.
            "reparented=rgb(70, 80, 90) 40px brand=rgb(70, 80, 90)",
            "reparentedNoOverride=rgb(40, 50, 60) brand=rgb(40, 50, 60)",
        ]
    );
}

// `getComputedStyle(el)` built a fresh `FunctionTemplate` and interceptor configuration
// for every declaration it handed back -- ~1,300 ns of a ~4,000 ns call, on a 12,004-element fixture
// where Chrome 150 makes the whole call in ~250 -- and gave every declaration its own hidden class.
//
// AS A COUNT, not a timing: the templates are built once per runtime however many declarations are
// minted, of either kind. And the speed-up changes no answer -- each call is still a NEW, LIVE object
// on the realm's own prototype (Chrome 150: `getComputedStyle(e) === getComputedStyle(e)` is false).
#[tokio::test]
async fn a_thousand_declarations_are_minted_from_one_template_build() {
    let before = webapi::declaration_template_builds();
    let out = run(
        r#"<!doctype html><html><head><style>.hot { color: rgb(1, 2, 3) }</style></head>
           <body><div id="d">x</div><p id="p">y</p></body></html>"#,
        r#"
        const el = document.getElementById("d");
        const P = CSSStyleDeclaration.prototype;
        const held = [];
        for (let i = 0; i < 1000; i++) held.push(getComputedStyle(i % 2 ? el : document.getElementById("p")));
        for (let i = 0; i < 1000; i++) document.createElement("i").style;
        print("fresh=" + (held[0] !== held[2]) + " " + (getComputedStyle(el) !== getComputedStyle(el)));
        print("instance=" + held.every(s => s instanceof CSSStyleDeclaration && Object.getPrototypeOf(s) === P));
        el.className = "hot";
        print("live=" + held[1].color + " " + held[999].color);

        // A page writing the global does not re-prototype what the engine mints (the rule,
        // and Chrome's answer).
        window.CSSStyleDeclaration = function Hijacked() {};
        print("sealed=" + (Object.getPrototypeOf(getComputedStyle(el)) === P));

        // The brand, not the prototype chain, makes an Element (Chrome 150, measured).
        Object.setPrototypeOf(el, null);
        let branded;
        try { branded = getComputedStyle(el).color; } catch (e) { branded = "threw " + e.message; }
        print("branded=" + branded);
        let notElement;
        try { getComputedStyle(document.createTextNode("t")); notElement = "no throw"; } catch (e) { notElement = e instanceof TypeError; }
        print("textThrowsTypeError=" + notElement);
        "#,
    )
    .await;
    let built = webapi::declaration_template_builds() - before;
    assert_eq!(
        out,
        vec![
            "fresh=true true",
            "instance=true",
            "live=rgb(1, 2, 3) rgb(1, 2, 3)",
            "sealed=true",
            "branded=rgb(1, 2, 3)",
            "textThrowsTypeError=true",
        ]
    );
    assert_eq!(built, 1, "2,000 declarations should come from ONE template build, not {built}");
}
