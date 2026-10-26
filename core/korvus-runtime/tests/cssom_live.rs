//! The CSSOM as ONE TABLE per interface: the four failure modes of a table cutover, and
//! the three defects the cutover removes, asked of a live engine.
//!
//! `tests/cssom.rs` is the BEHAVIOUR suite and predates this unit — it is unchanged, and it is what
//! proves the answers did not move. This file asks the questions a cutover has to answer and the
//! behaviour suite does not:
//!
//! 1. **An instance on the class template's prototype.** A rule, a sheet, a media list and a
//!    declaration block own NOTHING; every member is on the prototype where a browser has it.
//! 2. **A row accepting a foreign receiver.** Every row is branded on an internal slot, so a plain
//!    object, `Object.create(Iface.prototype)` and the prototype itself are all `Illegal invocation`
//!    — which is what Chrome answers, and what the deleted glue did not.
//! 3. **A body reachable where it was a silent no-op** (and the reverse).
//! 4. **An install ORDER change discarding a decorator** — `CSSStyleDeclaration.cssText` and
//!    `<style>/<link>.sheet` were the three members the kernel's bootstrap DECORATED after their
//!    tables had installed them. They are rows now, and they answer.

use engine::Engine;

async fn probe(html: &str, script: &str) -> Vec<String> {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(html).unwrap();
    e.eval("t", script.to_string()).unwrap();
    e.run().await.unwrap();
    e.take_output()
}

const PAGE: &str = r#"<!doctype html><html><head>
  <style id="s" media="print" title="t">h1 { color: red }</style>
  <link id="l" rel="stylesheet" href="/x.css">
</head><body></body></html>"#;

/// Failure mode 1: no instance carries a member the prototype should.
#[tokio::test]
async fn no_cssom_instance_owns_a_member() {
    let out = probe(
        PAGE,
        r#"
        const sheet = document.getElementById("s").sheet;
        const rule = sheet.cssRules[0];
        const decl = rule.style;
        const list = sheet.cssRules;
        print("sheet=" + Object.getOwnPropertyNames(sheet).length);
        print("rule=" + Object.getOwnPropertyNames(rule).length);
        // A declaration's indices and named CSS properties ARE its own (Blink's interceptors: stock Chrome 152 lists
        // 1 + 746 for this rule, Chrome 150 1 + 744) -- but no MEMBER of its prototype is.
        const own = Object.getOwnPropertyNames(decl);
        print("decl=" + own.filter((k) => Object.prototype.hasOwnProperty.call(CSSStyleDeclaration.prototype, k)).length + " " + own.length);
        // An index is not a member: `sheet.media[0]` is the list's own indexed property, as in Chrome 150
        // and every MEMBER is still on the prototype.
        print("media=" + Object.getOwnPropertyNames(sheet.media).filter((k) => !/^\d+$/.test(k)).length + " " + sheet.media[0]);
        // The two own properties the deleted glue left on every sheet, by name.
        print("sheetId=" + (sheet.__sheetId === undefined));
        print("ruleCache=" + (sheet.__ruleCache === undefined));
        print("json=" + JSON.stringify(sheet));
        print("list=" + (list instanceof CSSRuleList) + " " + Array.isArray(list));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "sheet=0", "rule=0", "decl=0 747", "media=0 print",
            "sheetId=true", "ruleCache=true", "json={}",
            "list=true false",
        ]
    );
}

/// Failure mode 2: every row rejects a foreign receiver, the way Chrome does.
#[tokio::test]
async fn a_foreign_receiver_is_an_illegal_invocation() {
    let out = probe(
        PAGE,
        r#"
        const why = (f) => { try { const v = f(); return "no throw:" + v; } catch (e) { return e.constructor.name + ":" + e.message; } };
        const d = (proto, name) => Object.getOwnPropertyDescriptor(proto, name);
        print("insertRule=" + why(() => CSSStyleSheet.prototype.insertRule.call({}, "a{}")));
        print("cssRules=" + why(() => d(CSSStyleSheet.prototype, "cssRules").get.call({})));
        print("type=" + why(() => d(StyleSheet.prototype, "type").get.call({})));
        print("selectorText=" + why(() => d(CSSStyleRule.prototype, "selectorText").get.call({})));
        print("ruleCssText=" + why(() => d(CSSRule.prototype, "cssText").get.call({})));
        print("mediaText=" + why(() => d(MediaList.prototype, "mediaText").get.call({})));
        // `Object.create(proto)` has the chain and NOT the slot -- the case a chain-only check lets
        // through and Chrome rejects.
        print("created=" + why(() => d(CSSStyleSheet.prototype, "cssRules").get.call(Object.create(CSSStyleSheet.prototype))));
        // The prototype itself is not an instance.
        print("proto=" + why(() => d(CSSStyleSheet.prototype, "cssRules").get.call(CSSStyleSheet.prototype)));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "insertRule=TypeError:Illegal invocation",
            "cssRules=TypeError:Illegal invocation",
            "type=TypeError:Illegal invocation",
            "selectorText=TypeError:Illegal invocation",
            "ruleCssText=TypeError:Illegal invocation",
            "mediaText=TypeError:Illegal invocation",
            "created=TypeError:Illegal invocation",
            "proto=TypeError:Illegal invocation",
        ]
    );
}

/// Every CSSOM member reports `[native code]` and carries no JS-shim tell. The deleted glue reached
/// the first half through `op_helper_get`/`op_helper_method` -- a native SHELL around a JS body,
/// which is the mask invariant I3 deletes. These bodies ARE Rust.
#[tokio::test]
async fn every_cssom_member_is_native() {
    let out = probe(
        PAGE,
        r#"
        const lies = [];
        const isNative = (f) => Function.prototype.toString.call(f).includes("[native code]");
        const tell = (f) => Object.prototype.hasOwnProperty.call(f, "prototype")
            || Object.prototype.hasOwnProperty.call(f, "arguments")
            || Object.prototype.hasOwnProperty.call(f, "caller");
        for (const n of ["StyleSheet", "CSSStyleSheet", "MediaList", "CSSRule", "CSSStyleRule",
                         "CSSPositionTryRule", "CSSCounterStyleRule", "CSSPositionTryDescriptors"]) {
            const C = globalThis[n];
            if (typeof C !== "function") { lies.push(n + ": absent"); continue; }
            if (!isNative(C)) lies.push(n + ": ctor not native");
            for (const m of Object.getOwnPropertyNames(C.prototype)) {
                // `constructor` is a CONSTRUCTOR: it owns `.prototype` in every browser, so the
                // shim-tell check does not apply to it. Measured in headless Chrome 141:
                // `Object.prototype.hasOwnProperty.call(CSSRule.prototype.constructor, "prototype")`
                // is `true`.
                if (m === "constructor") continue;
                const d = Object.getOwnPropertyDescriptor(C.prototype, m);
                for (const [k, f] of [["get", d.get], ["set", d.set], ["value", typeof d.value === "function" ? d.value : null]]) {
                    if (!f) continue;
                    if (!isNative(f)) lies.push(n + "." + m + " " + k + ": not native");
                    if (tell(f)) lies.push(n + "." + m + " " + k + ": shim tell");
                }
            }
        }
        // `<style>/<link>.sheet` and `CSSStyleDeclaration.cssText` -- the three the bootstrap decorated.
        for (const [n, m] of [["HTMLStyleElement", "sheet"], ["HTMLLinkElement", "sheet"],
                              ["SVGStyleElement", "sheet"], ["CSSStyleDeclaration", "cssText"]]) {
            const d = Object.getOwnPropertyDescriptor(globalThis[n].prototype, m);
            if (!d || !d.get) { lies.push(n + "." + m + ": no getter"); continue; }
            if (!isNative(d.get)) lies.push(n + "." + m + ": not native");
            if (tell(d.get)) lies.push(n + "." + m + ": shim tell");
        }
        print(JSON.stringify(lies));
        "#,
    )
    .await;
    assert_eq!(out, vec!["[]"]);
}

/// The WebIDL chain, including the fourteen registry rule shells whose parent left the registry with
/// `CSSRule` -- `rechain_orphans` is what repairs those, and a broken chain is invisible until asked.
#[tokio::test]
async fn the_cssom_prototype_chain_is_webidls() {
    let out = probe(
        PAGE,
        r#"
        const p = (a, b) => print(a + "->" + b + "=" + (Object.getPrototypeOf(globalThis[a].prototype) === globalThis[b].prototype));
        p("CSSStyleSheet", "StyleSheet");
        p("CSSStyleRule", "CSSRule");
        p("CSSPositionTryRule", "CSSRule");
        p("CSSCounterStyleRule", "CSSRule");
        p("CSSPositionTryDescriptors", "CSSStyleDeclaration");
        // WebIDL §3.7: the interface OBJECT inherits too.
        print("ctor=" + (Object.getPrototypeOf(CSSStyleSheet) === StyleSheet));
        // The registry's own rule shells, orphaned when CSSRule left the registry.
        const orphans = ["CSSFontFaceRule", "CSSImportRule", "CSSKeyframeRule", "CSSKeyframesRule",
                         "CSSGroupingRule", "CSSMarginRule", "CSSNamespaceRule", "CSSPropertyRule",
                         "CSSNestedDeclarations", "CSSViewTransitionRule", "CSSLayerStatementRule",
                         "CSSFontPaletteValuesRule", "CSSFontFeatureValuesRule"];
        print("orphans=" + orphans.filter((n) => globalThis[n]
            && Object.getPrototypeOf(globalThis[n].prototype) !== CSSRule.prototype).join(",") + "|");
        print("newable=" + (new CSSStyleSheet() instanceof CSSStyleSheet));
        print("illegal=" + (() => { try { new CSSRule(); return "no throw"; } catch (e) { return e.constructor.name; } })());
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "CSSStyleSheet->StyleSheet=true",
            "CSSStyleRule->CSSRule=true",
            "CSSPositionTryRule->CSSRule=true",
            "CSSCounterStyleRule->CSSRule=true",
            "CSSPositionTryDescriptors->CSSStyleDeclaration=true",
            "ctor=true",
            "orphans=|",
            "newable=true",
            "illegal=TypeError",
        ]
    );
}

/// `StyleSheet`'s seven members, which were SEVEN OWN DATA PROPERTIES on every sheet. `media` was a
/// plain object literal; it is a `MediaList` over the element's `media` attribute now, so
/// `<style media="print">` finally says `print`.
#[tokio::test]
async fn a_sheet_answers_the_style_sheet_members_from_its_prototype() {
    let out = probe(
        PAGE,
        r#"
        const s = document.getElementById("s").sheet;
        print("type=" + s.type);
        print("owner=" + (s.ownerNode === document.getElementById("s")));
        print("parent=" + s.parentStyleSheet);
        print("title=" + s.title);
        print("href=" + s.href);
        print("disabled=" + s.disabled);
        s.disabled = true; print("disabled2=" + s.disabled);
        print("media=" + (s.media instanceof MediaList) + " " + s.media.mediaText + " " + s.media.length + " " + s.media.item(0) + " " + s.media.item(9));
        print("same=" + (s.media === s.media));
        s.media.appendMedium("screen");
        print("appended=" + s.media.mediaText + " attr=" + document.getElementById("s").getAttribute("media"));
        s.media.deleteMedium("print");
        print("deleted=" + s.media.mediaText + " " + String(s.media));
        const l = document.getElementById("l").sheet;
        print("linkHref=" + (typeof l.href === "string" && l.href.endsWith("/x.css")));
        print("linkTitle=" + l.title);
        // A CONSTRUCTED sheet takes `media` and `disabled` from its init and answers `title` NULL
        // anyway. Measured in headless Chrome 141, 2026-09-12:
        // `new CSSStyleSheet({media:"screen",title:"c",disabled:true})` -> `screen null true null null null`.
        const c = new CSSStyleSheet({ media: "screen", title: "c", disabled: true });
        print("ctor=" + c.media.mediaText + " " + c.title + " " + c.disabled + " " + c.ownerNode + " " + c.href + " " + c.ownerRule);
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "type=text/css",
            "owner=true",
            "parent=null",
            "title=t",
            "href=null",
            "disabled=false",
            "disabled2=true",
            "media=true print 1 print null",
            "same=true",
            // The media list is the SHEET's: appending to it leaves the element's `media` attribute alone.
            // Asked of stock Chrome 150.0.7871.46 with this exact probe and page: `attr=print`.
            "appended=print, screen attr=print",
            "deleted=screen screen",
            "linkHref=true",
            "linkTitle=null",
            "ctor=screen null true null null null",
        ]
    );
}

/// Failure mode 3, both directions. `rule.style.cssText = "…"` on a STYLE rule was a silent no-op:
/// the deleted glue routed every block write through `op_css_set_descriptor`, which answers `false`
/// for a style rule, so the assignment did nothing and reported nothing. And `cssText` on a rule
/// itself stays a no-op, because CSSOM says so ("on setting, do nothing").
#[tokio::test]
async fn writing_a_rules_block_reparses_the_rule() {
    let out = probe(
        PAGE,
        r#"
        const s = document.getElementById("s").sheet;
        const r = s.cssRules[0];
        print("before=" + r.style.cssText);
        r.style.cssText = "color: blue";
        print("after=" + r.style.cssText + " count=" + s.cssRules.length + " sel=" + s.cssRules[0].selectorText);
        // Same rule object: the index did not move, so nothing the page is holding went stale.
        print("same=" + (s.cssRules[0] === r));
        // [PutForwards=cssText] on the attribute itself.
        r.style = "color: teal";
        print("forwards=" + r.style.cssText);
        // CSSOM's `cssText` setter EMPTIES the declarations first and then parses, so text that is
        // not a declaration list leaves an EMPTY block -- it does not leave the old one standing.
        // Measured in headless Chrome 141, 2026-09-12: `r.style.cssText = "}} not css {{"` then
        // `r.style.cssText` is `""`.
        r.style.cssText = "}} not css {{";
        print("garbage=" + JSON.stringify(r.style.cssText));
        // CSSOM: setting a RULE's cssText does nothing.
        const was = r.cssText;
        r.cssText = "p { color: pink }";
        print("ruleCssText=" + (r.cssText === was));
        print("type=" + r.type + " " + CSSRule.STYLE_RULE + " " + CSSRule.prototype.STYLE_RULE);
        print("parentSheet=" + (r.parentStyleSheet === s) + " parentRule=" + r.parentRule);
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "before=color: red;",
            "after=color: blue; count=1 sel=h1",
            "same=true",
            "forwards=color: teal;",
            "garbage=\"\"",
            "ruleCssText=true",
            "type=1 1 1",
            "parentSheet=true parentRule=null",
        ]
    );
}

/// The legacy pair CSSOM still defines, which were registry `stub` rows answering `undefined`.
#[tokio::test]
async fn add_rule_and_remove_rule_are_the_legacy_members_cssom_defines() {
    let out = probe(
        PAGE,
        r#"
        const s = new CSSStyleSheet();
        s.replaceSync("a { color: red }");
        print("add=" + s.addRule("b", "color: blue"));
        print("count=" + s.cssRules.length + " last=" + s.cssRules[1].selectorText + " " + s.cssRules[1].style.cssText);
        s.removeRule(0);
        print("removed=" + s.cssRules.length + " " + s.cssRules[0].selectorText);
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec!["add=-1", "count=2 last=b color: blue;", "removed=1 b"]
    );
}

/// `document.styleSheets` still walks to the same sheets, and an SVG `<style>` still has one --
/// `SVGStyleElement`'s own `sheet` row, over `HTMLStyleElement`'s body.
#[tokio::test]
async fn document_style_sheets_and_the_svg_style_element() {
    let out = probe(
        r#"<!doctype html><html><head><style>h1{color:red}</style></head>
           <body><svg><style>#r{fill:green}</style><rect id="r"></rect></svg></body></html>"#,
        r#"
        print("count=" + document.styleSheets.length);
        const svgStyle = document.querySelector("svg style");
        print("svgIface=" + svgStyle.constructor.name);
        print("svgSheet=" + (svgStyle.sheet instanceof CSSStyleSheet));
        print("svgRules=" + svgStyle.sheet.cssRules.length + " " + svgStyle.sheet.cssRules[0].selectorText);
        // A DISCONNECTED element has no CSS style sheet.
        print("fresh=" + document.createElement("style").sheet);
        print("freshLink=" + document.createElement("link").sheet);
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec!["count=2", "svgIface=SVGStyleElement", "svgSheet=true", "svgRules=1 #r", "fresh=null", "freshLink=null"]
    );
}
