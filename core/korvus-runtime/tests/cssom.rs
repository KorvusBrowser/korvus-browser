//! `CSSStyleSheet` read through to korvus-page's parser.
//!
//! Before this, a sheet was a list of `{cssText, type}` blobs and `replaceSync` split the text on
//! `}` — so `selectorText` did not exist, `cssText` echoed whatever the page wrote, and an invalid
//! selector was indistinguishable from a valid one. These are the behaviours a page can tell apart.
use engine::Engine;
async fn run(script: &str) -> Vec<String> {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(
        r#"<!doctype html><html><head><style id="s">
             h1, .lede { color: red; margin: 0 auto }
             p { }
           </style></head><body></body></html>"#,
    )
    .unwrap();
    e.eval("t", script.to_string()).unwrap();
    e.run().await.unwrap();
    e.take_output()
}

#[tokio::test]
async fn a_sheet_reads_back_as_parsed_rules() {
    let out = run(r#"
        const sheet = document.getElementById("s").sheet;
        print("count=" + sheet.cssRules.length);
        print("selector=" + sheet.cssRules[0].selectorText);
        print("text=" + sheet.cssRules[0].cssText);
        print("style=" + sheet.cssRules[0].style.cssText);
        print("empty=" + sheet.cssRules[1].cssText);
    "#)
    .await;
    assert_eq!(
        out,
        vec![
            "count=2",
            "selector=h1, .lede",
            // The shorthand is stored as the longhands it expands to and written back as the shorthand,
            // in Blink's spelling (Chrome 150, `style::serialize`).
            "text=h1, .lede { color: red; margin: 0px auto; }",
            "style=color: red; margin: 0px auto;",
            "empty=p { }",
        ]
    );
}

// `sheet.cssRules[0] === sheet.cssRules[0]` is something pages check.
#[tokio::test]
async fn rule_objects_keep_their_identity_between_reads() {
    let out = run(r#"
        const sheet = document.getElementById("s").sheet;
        print("same=" + (sheet.cssRules[0] === sheet.cssRules[0]));
        print("proto=" + (sheet.cssRules[0] instanceof CSSStyleRule));
        print("parent=" + (sheet.cssRules[0].parentStyleSheet === sheet));
    "#)
    .await;
    assert_eq!(out, vec!["same=true", "proto=true", "parent=true"]);
}

// The path CSS-in-JS takes on every render.
#[tokio::test]
async fn insert_and_delete_move_the_rules_the_page_can_see() {
    let out = run(r#"
        const sheet = document.getElementById("s").sheet;
        print("at=" + sheet.insertRule("b { color: blue }", 0));
        print("first=" + sheet.cssRules[0].selectorText + " count=" + sheet.cssRules.length);
        // `insertRule(rule)` WITH NO INDEX INSERTS AT 0, and returns 0. CSSOM says
        // `optional unsigned long index = 0`; this engine used to APPEND, and this case recorded that.
        // Asked of `google-chrome --headless=new` 141 on 2026-09-12, on a sheet holding one rule:
        // `c.insertRule("b{color:blue}")` -> `0`, and `c.cssRules[0].selectorText` -> `"b"`.
        // (`addRule` is the one that defaults to the END -- also measured, also what CSSOM says.)
        print("noIndex=" + sheet.insertRule("i { color: teal }"));
        print("atZero=" + sheet.cssRules[0].selectorText + " count=" + sheet.cssRules.length);
        sheet.deleteRule(0);
        print("afterDelete=" + sheet.cssRules[0].selectorText + " count=" + sheet.cssRules.length);
    "#)
    .await;
    assert_eq!(
        out,
        vec!["at=0", "first=b count=3", "noIndex=0", "atZero=i count=4", "afterDelete=b count=3"]
    );
}

/// THE EXCEPTION IS A `DOMException`, NOT A `TypeError`, and its `name` and `message` are the CSSOM's.
/// This engine threw `TypeError` for both, so a page's `catch (e) { if (e.name === "IndexSizeError") }`
/// took the wrong branch. Asked of `google-chrome --headless=new` 141 on 2026-09-12, on the same
/// one-rule `<style>` sheet:
///
/// ```text
/// s.insertRule("b{color:blue}", 99)   DOMException / IndexSizeError
///   "Failed to execute 'insertRule' on 'CSSStyleSheet': The index provided (99) is larger than the
///    maximum index (1)."
/// s.insertRule("this is not css", 0)  DOMException / SyntaxError
///   "Failed to execute 'insertRule' on 'CSSStyleSheet': Failed to parse the rule 'this is not css'."
/// s.insertRule("b{color:blue}", -1)   DOMException / IndexSizeError, index reported as 4294967295
///   -- WebIDL `unsigned long` is ToUint32, so a negative index is a LARGE one, not its own failure.
/// ```
#[tokio::test]
async fn insert_rule_throws_where_a_browser_throws() {
    let out = run(r#"
        const sheet = document.getElementById("s").sheet;
        const why = (f) => { try { f(); return "no throw"; } catch (e) { return e.constructor.name + "/" + e.name; } };
        const msg = (f) => { try { f(); return "no throw"; } catch (e) { return e.message; } };
        print("oob=" + why(() => sheet.insertRule("b { color: blue }", 99)));
        print("oobMsg=" + msg(() => sheet.insertRule("b { color: blue }", 99)));
        print("syntax=" + why(() => sheet.insertRule("this is not css", 0)));
        print("syntaxMsg=" + msg(() => sheet.insertRule("this is not css", 0)));
        print("negative=" + msg(() => sheet.insertRule("b { color: blue }", -1)));
        print("unchanged=" + sheet.cssRules.length);
    "#)
    .await;
    assert_eq!(
        out,
        vec![
            "oob=DOMException/IndexSizeError",
            "oobMsg=Failed to execute 'insertRule' on 'CSSStyleSheet': The index provided (99) is larger than the maximum index (2).",
            "syntax=DOMException/SyntaxError",
            "syntaxMsg=Failed to execute 'insertRule' on 'CSSStyleSheet': Failed to parse the rule 'this is not css'.",
            "negative=Failed to execute 'insertRule' on 'CSSStyleSheet': The index provided (4294967295) is larger than the maximum index (2).",
            "unchanged=2",
        ]
    );
}

// An invalid selector leaves the rule alone: not an exception, and not a blank selector.
#[tokio::test]
async fn setting_selector_text_takes_valid_selectors_and_ignores_the_rest() {
    let out = run(r#"
        const rule = document.getElementById("s").sheet.cssRules[0];
        rule.selectorText = "q, #r";
        print("set=" + rule.selectorText);
        for (const bad of ["", " ", "!!", "123"]) {
            rule.selectorText = bad;
        }
        print("stillSet=" + rule.selectorText);
    "#)
    .await;
    assert_eq!(out, vec!["set=q, #r", "stillSet=q, #r"]);
}

/// `replaceSync` IS CONSTRUCTED-SHEET-ONLY. This case used to call it on a `<style>` element's sheet,
/// where a browser refuses. Asked of `google-chrome --headless=new` 141 on 2026-09-12:
///
/// ```text
/// document.getElementById("s").sheet.replaceSync("z{color:pink}")
///   DOMException / NotAllowedError: "Failed to execute 'replaceSync' on 'CSSStyleSheet': Can't call
///    replaceSync on non-constructed CSSStyleSheets."
/// ```
///
/// So the re-parse is asked of a CONSTRUCTED sheet, which is what the member is for, and the refusal
/// is asserted on the element's. A `<style>`'s sheet still follows its text -- that path is the
/// element `sheet` row's own re-sync, not this member (see the next case).
#[tokio::test]
async fn replace_sync_reparses_the_whole_sheet() {
    let out = run(r#"
        const sheet = new CSSStyleSheet();
        sheet.replaceSync("z { color: pink } y { color: gold }");
        print("count=" + sheet.cssRules.length);
        print("first=" + sheet.cssRules[0].cssText);
        sheet.replaceSync("");
        print("emptied=" + sheet.cssRules.length);
        const owned = document.getElementById("s").sheet;
        try { owned.replaceSync("z { color: pink }"); print("owned=no throw"); }
        catch (e) { print("owned=" + e.constructor.name + "/" + e.name + ": " + e.message); }
        print("ownedKept=" + owned.cssRules.length);
        owned.replace("z { color: pink }").then(
            () => print("replaceAsync=resolved"),
            (e) => print("replaceAsync=" + e.name));
    "#)
    .await;
    assert_eq!(
        out,
        vec![
            "count=2",
            "first=z { color: pink; }",
            "emptied=0",
            "owned=DOMException/NotAllowedError: Failed to execute 'replaceSync' on 'CSSStyleSheet': Can't call replaceSync on non-constructed CSSStyleSheets.",
            "ownedKept=2",
            "replaceAsync=NotAllowedError",
        ]
    );
}

// The reason any of this exists: emotion and styled-components insert during React's render, and a
// sheet that throws there blanks the page.
#[tokio::test]
async fn a_css_in_js_style_insert_loop_works() {
    let out = run(r#"
        const el = document.createElement("style");
        document.head.appendChild(el);
        const sheet = el.sheet;
        for (let i = 0; i < 50; i++) sheet.insertRule(".c" + i + " { color: red }", sheet.cssRules.length);
        print("count=" + sheet.cssRules.length);
        print("last=" + sheet.cssRules[49].selectorText);
    "#)
    .await;
    assert_eq!(out, vec!["count=50", "last=.c49"]);
}

// A `<style>` element's sheet is a view of its text, not a snapshot: this is how a good deal of
// WPT drives the CSSOM, and how a page that swaps a theme by rewriting a style block works.
#[tokio::test]
async fn rewriting_a_style_elements_text_reparses_its_sheet() {
    let out = run(r#"
        const el = document.getElementById("s");
        el.firstChild.data = "z { color: pink }";
        print("after=" + el.sheet.cssRules.length + " " + el.sheet.cssRules[0].selectorText);
        el.textContent = "a { color: red } b { color: blue }";
        print("again=" + el.sheet.cssRules.length);
        // Reading twice without touching the text must not re-parse away an edit.
        el.sheet.insertRule("i { color: teal }", 0);
        print("kept=" + el.sheet.cssRules.length + " " + el.sheet.cssRules[0].selectorText);
    "#)
    .await;
    assert_eq!(out, vec!["after=1 z", "again=2", "kept=3 i"]);
}

// `@position-try` and `@counter-style` used to be skipped by the parser, so a sheet whose only rule
// was one of them had no rules at all: `sheet.cssRules[0]` was `undefined` and every read off it
// threw where a browser answers a string.
#[tokio::test]
async fn a_position_try_rule_is_a_rule_with_a_descriptors_object() {
    let out = run(r#"
        const s = new CSSStyleSheet();
        s.replaceSync("@position-try --p { top: 0px }");
        print("count=" + s.cssRules.length);
        print("brand=" + Object.prototype.toString.call(s.cssRules[0]));
        print("name=" + s.cssRules[0].name);
        const o = s.cssRules[0].style;
        print("style=" + Object.prototype.toString.call(o));
        print("same=" + (s.cssRules[0].style === o));
        print("top=" + o.top);
        print("margin=" + o.margin);
        print("marginTop=" + o.marginTop);
        // An assignment the grammar rejects is dropped, and the descriptor keeps what it had.
        o.top = "x"; o.marginTop = "x";
        print("afterTop=" + o.top);
        print("afterMarginTop=" + o.marginTop);
        // One it accepts sticks, and the shorthand rebuilds itself from the longhands.
        o.margin = "1px 2px";
        print("margin2=" + o.margin + "|" + o.marginTop + "|" + o.marginRight);
        // The descriptors live on the prototype, not on the instance: a page enumerating the object
        // must see nothing, exactly as it does in a browser.
        print("own=" + Object.getOwnPropertyNames(o).length);
    "#)
    .await;
    assert_eq!(
        out,
        vec![
            "count=1",
            "brand=[object CSSPositionTryRule]",
            "name=--p",
            "style=[object CSSPositionTryDescriptors]",
            "same=true",
            "top=0px",
            "margin=",
            "marginTop=",
            "afterTop=0px",
            "afterMarginTop=",
            "margin2=1px 2px|1px|2px",
            "own=0",
        ]
    );
}

#[tokio::test]
async fn a_counter_style_rule_answers_its_descriptors() {
    let out = run(r#"
        const s = new CSSStyleSheet();
        s.replaceSync("@counter-style c { system: cyclic; symbols: x; suffix: '.' }");
        const o = s.cssRules[0];
        print("brand=" + Object.prototype.toString.call(o));
        print("name=" + o.name);
        print("system=" + o.system);
        print("symbols=" + o.symbols);
        print("suffix=" + o.suffix);
        print("range=" + o.range);
        o.name = "x"; print("name2=" + o.name);
        o.system = "x"; print("system2=" + o.system);
        o.negative = "x"; print("negative=" + o.negative);
        o.pad = "x"; print("pad=" + o.pad);
        o.speakAs = "x"; print("speakAs=" + o.speakAs);
    "#)
    .await;
    assert_eq!(
        out,
        vec![
            "brand=[object CSSCounterStyleRule]",
            "name=c",
            "system=cyclic",
            "symbols=x",
            "suffix=\".\"",
            "range=",
            "name2=x",
            "system2=cyclic",
            "negative=x",
            "pad=",
            "speakAs=x",
        ]
    );
}
