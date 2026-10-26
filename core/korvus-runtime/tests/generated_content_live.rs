//! pseudo-elements, `@container`/`@scope`/`@property`/`@page`, quirks mode, and the
//! three things a page shows that it should not (`<noscript>`, `[hidden]`, a closed `<details>`), in a
//! live engine over a loopback server whose access log is the truth.
//!
//! # The oracle
//!
//! Every expected value below is stock Google Chrome 150.0.7871.46's (`--headless=new`, 780x600
//! window, 2026-09-17), read over CDP from the SAME markup served by a loopback server that logged
//! every request (the unit's `pseudo.html`, `atrules.html`, `q2.html` and the quirks bodies).
//! Where Chrome's number depends on its viewport the probe asserts the relation instead
//! (`documentElement.offsetHeight == innerHeight` rather than 513).
//!
//! What is deliberately NOT asserted, each for a named reason outside this unit:
//!
//! ```text
//! getComputedStyle(el).length             Chrome 474, korvus 475 -- for the ELEMENT too, before and
//!                                         after this unit (the worn bundle's property table);
//!                                         asserted instead: a pseudo-element's length equals it
//! a clearfix ::after around a float       Chrome 40, korvus 44 -- floats are atomic inlines here
//! open-quote "x" close-quote width        Chrome 18.66, korvus 25.8 -- no metrics for U+201C/D
//! CSS.registerProperty                    absent from every captured bundle's surface
//! td padding/margin inside a table        tables lay out as blocks here
//! ```

use engine::Engine;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

type Log = Arc<Mutex<Vec<String>>>;

const PSEUDO: &str = r##"<!doctype html>
<meta charset="utf-8">
<style>
body { font: 16px/18px Arial; margin: 8px }
#a::before { content: "PRE"; display: inline-block; width: 40px; height: 24px }
#b::before { content: ""; display: block; width: 10px; height: 10px; background: url(pseudo-bg.png) }
#b::after { content: url(pseudo-content.png) }
#c::after { content: attr(data-x) " " counter(n) " " open-quote "q" close-quote }
#d::before { content: "X"; font-family: PseudoFace }
@font-face { font-family: PseudoFace; src: url(pseudo-font.woff2) format("woff2") }
#h::before { content: "never"; background-image: url(pseudo-none.png) }
#h { display: none }
#nc::before { background-image: url(pseudo-nocontent.png); width: 10px; height: 10px; display: block }
#blk::before { content: ""; display: block; height: 30px }
#blk::after { content: ""; display: block; height: 5px }
#flx { display: flex }
#flx::before { content: ""; width: 20px; height: 20px }
#sp::before { content: "abc" }
li::marker { color: rgb(1, 2, 3) }
input::placeholder { color: rgb(4, 5, 6) }
p#fl::first-letter { color: rgb(7, 8, 9); font-size: 32px }
p#fl::first-line { color: rgb(10, 11, 12) }
p#fl::selection { color: rgb(13, 14, 15); background-color: rgb(16, 17, 18) }
#sc { width: 200px; height: 100px; overflow: scroll }
#sc::-webkit-scrollbar { width: 8px; height: 6px }
#ov { width: 200px; height: 100px; overflow: overlay }
#none::before { color: red }
#abs { position: relative; width: 100px; height: 20px }
#abs::after { content: ""; position: absolute; left: 0; top: 0; width: 7px; height: 70px }
</style>
<body>
<div id=a>text</div>
<div id=b>b</div>
<div id=c data-x="ATTR">c</div>
<div id=d>d</div>
<div id=h>h</div>
<div id=nc>nc</div>
<div id=blk>mid</div>
<div id=flx><div style="width:10px;height:10px"></div></div>
<span id=sp>xyz</span>
<ul><li id=li>item</li></ul>
<input id=in placeholder="ph">
<p id=fl>first letter line</p>
<div id=sc><div style="height:300px;width:10px"></div></div>
<div id=ov><div style="height:300px;width:10px"></div></div>
<div id=none>none</div>
<div id=abs>abs</div>
<textarea id=ta></textarea>
</body>"##;

const PSEUDO_PROBE: &str = r##"
const q = (id) => document.getElementById(id);
const g = (id, p, prop) => getComputedStyle(q(id), p)[prop];
const out = (k, v) => print(k + "=" + v);
out("a_before_content", g("a", "::before", "content"));
out("a_before_width", g("a", "::before", "width"));
out("a_before_height", g("a", "::before", "height"));
out("a_before_display", g("a", "::before", "display"));
out("a_before_len_is_the_elements", getComputedStyle(q("a"), "::before").length === getComputedStyle(q("a")).length);
out("a_height", q("a").offsetHeight);
out("a_innerText", q("a").innerText);
out("a_textContent", q("a").textContent);
out("a_children", q("a").childNodes.length);
out("a_after_content", g("a", "::after", "content"));
out("a_after_width", g("a", "::after", "width"));
out("a_after_display", g("a", "::after", "display"));
out("a_single_colon", g("a", ":before", "content"));
out("nonsense_len", getComputedStyle(q("a"), "::nonsense").length);
out("b_before_bg", g("b", "::before", "backgroundImage").replace(location.host, "HOST"));
out("b_after_content", g("b", "::after", "content").replace(location.host, "HOST"));
out("b_height", q("b").offsetHeight);
out("c_after_content", g("c", "::after", "content"));
out("c_innerText", q("c").innerText);
out("nc_height", q("nc").offsetHeight);
out("blk_height", q("blk").offsetHeight);
out("flx_height", q("flx").offsetHeight);
out("flx_before_display", g("flx", "::before", "display"));
out("sp_before_content", g("sp", "::before", "content"));
out("li_marker_color", g("li", "::marker", "color"));
out("li_marker_content", g("li", "::marker", "content"));
out("in_placeholder_color", g("in", "::placeholder", "color"));
out("fl_first_letter_color", g("fl", "::first-letter", "color"));
out("fl_first_letter_size", g("fl", "::first-letter", "fontSize"));
out("fl_first_line_color", g("fl", "::first-line", "color"));
out("fl_selection_color", g("fl", "::selection", "color"));
out("fl_selection_bg", g("fl", "::selection", "backgroundColor"));
out("sc_client", q("sc").clientWidth + "x" + q("sc").clientHeight);
out("sc_sb_width", g("sc", "::-webkit-scrollbar", "width"));
out("ov_overflow", g("ov", null, "overflow"));
out("ov_client", q("ov").clientWidth + "x" + q("ov").clientHeight);
out("ta_overflow", g("ta", null, "overflow"));
out("none_before_color", g("none", "::before", "color"));
out("none_before_content", g("none", "::before", "content"));
out("abs_scroll", q("abs").scrollHeight);
out("qsa_before", (() => { try { return document.querySelectorAll("#a::before").length } catch (e) { return e.name } })());
out("qsa_wk", (() => { try { return document.querySelectorAll("::-webkit-scrollbar").length } catch (e) { return e.name } })());
out("wk_rule", (() => { const s = new CSSStyleSheet(); try { s.insertRule("::-webkit-scrollbar-thumb { color: red }"); return s.cssRules[0].selectorText } catch (e) { return e.name } })());
out("wk_unknown", (() => { const s = new CSSStyleSheet(); try { s.insertRule("::-webkit-foo-bar { color: red }"); return s.cssRules[0].selectorText } catch (e) { return e.name } })());
"##;

/// Chrome 150's answers to [`PSEUDO_PROBE`] over [`PSEUDO`].
const PSEUDO_CHROME: &[&str] = &[
    "a_before_content=\"PRE\"",
    "a_before_width=40px",
    "a_before_height=24px",
    "a_before_display=inline-block",
    "a_before_len_is_the_elements=true",
    "a_height=24",
    "a_innerText=text",
    "a_textContent=text",
    "a_children=1",
    "a_after_content=none",
    "a_after_width=auto",
    "a_after_display=inline",
    "a_single_colon=\"PRE\"",
    "nonsense_len=0",
    "b_before_bg=url(\"http://HOST/pseudo-bg.png\")",
    "b_after_content=url(\"http://HOST/pseudo-content.png\")",
    "b_height=28",
    "c_after_content=\"ATTR \" counter(n) \" \" open-quote \"q\" close-quote",
    "c_innerText=c",
    "nc_height=18",
    "blk_height=53",
    "flx_height=20",
    "flx_before_display=block",
    "sp_before_content=\"abc\"",
    "li_marker_color=rgb(1, 2, 3)",
    "li_marker_content=normal",
    "in_placeholder_color=rgb(4, 5, 6)",
    "fl_first_letter_color=rgb(7, 8, 9)",
    "fl_first_letter_size=32px",
    "fl_first_line_color=rgb(10, 11, 12)",
    "fl_selection_color=rgb(13, 14, 15)",
    "fl_selection_bg=rgb(16, 17, 18)",
    "sc_client=192x94",
    "sc_sb_width=8px",
    "ov_overflow=auto",
    "ov_client=185x100",
    "ta_overflow=auto",
    "none_before_color=rgb(255, 0, 0)",
    "none_before_content=none",
    "abs_scroll=70",
    "qsa_before=0",
    "qsa_wk=0",
    "wk_rule=::-webkit-scrollbar-thumb",
    "wk_unknown=::-webkit-foo-bar",
];

const ATRULES: &str = r##"<!doctype html>
<meta charset="utf-8">
<style>
@property --x { syntax: "<length>"; inherits: false; initial-value: 7px }
@property --inh { syntax: "<length>"; inherits: true; initial-value: 3px }
@property --col { syntax: "<color>"; inherits: false; initial-value: red }
@property --any { syntax: "*"; inherits: false }
.cq { container-type: inline-size; width: 300px }
.cqn { container: card / inline-size; width: 150px }
@container (min-width: 200px) { .cqc { color: rgb(9, 9, 9) } }
@container (min-width: 400px) { .cqx { color: rgb(99, 99, 99) } }
@container card (width < 200px) { .named { color: rgb(19, 19, 19) } }
@container style(--t: 1) { .sty { color: rgb(29, 29, 29) } }
.cqw { width: 50cqw; height: 1px }
@scope (.sc) { p { color: rgb(8, 8, 8) } :scope { background-color: rgb(1, 1, 1) } }
@scope (.sc2) to (.stop) { p { color: rgb(18, 18, 18) } }
@page :first { margin: 1in }
@counter-style thumbs { system: cyclic; symbols: "*"; suffix: " " }
.px { width: var(--x); height: 1px }
.pem { --x: 2em; font-size: 10px; width: var(--x); height: 1px }
.pbad { --x: red; width: var(--x); height: 1px }
.pcol { --col: #00ff00 }
.pinh { --inh: 5px; --x: 11px }
@property --bad { syntax: "<length>"; inherits: false }
</style>
<body>
<div class=cq><div class=cqc id=cqc>c</div><div class=cqx id=cqx>x</div><div class=cqw id=cqw></div></div>
<div class=cqn><div class=named id=named>n</div></div>
<div style="--t: 1"><div class=sty id=sty>s</div></div>
<div class=sc id=sc><p id=scp>p</p></div>
<div class=sc2><p id=sc2p>p</p><div class=stop><p id=stopp>p</p></div></div>
<p id=outp>out</p>
<div class=px id=px></div>
<div class=pem id=pem></div>
<div class=pbad id=pbad></div>
<div class=pcol id=pcol></div>
<div class=pinh id=pinh><div id=pinhc></div></div>
</body>"##;

const ATRULES_PROBE: &str = r##"
const q = (id) => document.getElementById(id);
const cs = (id) => getComputedStyle(q(id));
const out = (k, v) => print(k + "=" + v);
out("cqc", cs("cqc").color);
out("cqx", cs("cqx").color);
out("named", cs("named").color);
out("sty", cs("sty").color);
out("cqw_width", cs("cqw").width);
out("cq_type", getComputedStyle(document.querySelector(".cq")).containerType);
out("cqn_name", getComputedStyle(document.querySelector(".cqn")).containerName);
out("scp", cs("scp").color);
out("sc_bg", cs("sc").backgroundColor);
out("sc2p", cs("sc2p").color);
out("stopp", cs("stopp").color);
out("outp", cs("outp").color);
out("px_width", cs("px").width);
out("px_var", cs("px").getPropertyValue("--x"));
out("pem_var", cs("pem").getPropertyValue("--x"));
out("pem_width", cs("pem").width);
out("pbad_var", cs("pbad").getPropertyValue("--x"));
out("pbad_width", cs("pbad").width);
out("pcol_var", cs("pcol").getPropertyValue("--col"));
out("pinh_x", cs("pinhc").getPropertyValue("--x"));
out("pinh_inh", cs("pinhc").getPropertyValue("--inh"));
out("body_inh", getComputedStyle(document.body).getPropertyValue("--inh"));
out("any", getComputedStyle(document.body).getPropertyValue("--any"));
const rules = [...document.styleSheets[0].cssRules];
out("ctors", rules.map((x) => x.constructor.name).join(","));
out("types", rules.map((x) => x.type).join(","));
const prop = rules[0];
out("prop", [prop.name, prop.syntax, prop.inherits, prop.initialValue, prop.cssText].join("|"));
out("anyprop", [rules[3].initialValue, rules[3].cssText].join("|"));
const cont = rules.find((x) => x.constructor.name === "CSSContainerRule");
out("cont", [cont.conditionText, cont.containerName, cont.containerQuery, cont.cssRules.length, cont.cssText].join("|"));
const named = rules.filter((x) => x.constructor.name === "CSSContainerRule")[2];
out("cont_named", [named.conditionText, named.containerName, named.containerQuery].join("|"));
const scope = rules.find((x) => x.constructor.name === "CSSScopeRule");
out("scope", [scope.start, scope.end, scope.cssRules.length, scope.cssText].join("|"));
const scope2 = rules.filter((x) => x.constructor.name === "CSSScopeRule")[1];
out("scope2", [scope2.start, scope2.end, scope2.cssText].join("|"));
const page = rules.find((x) => x.constructor.name === "CSSPageRule");
out("page", [page.selectorText, page.style.margin, page.cssText].join("|"));
const cstyle = rules.find((x) => x.constructor.name === "CSSCounterStyleRule");
out("cstyle", [cstyle.name, cstyle.system, cstyle.symbols, cstyle.suffix, cstyle.cssText].join("|"));
out("ins", (() => { const s = new CSSStyleSheet(); try { s.insertRule("@container (min-width: 1px) { a { color: red } }"); s.insertRule("@scope (.a) { b { color: red } }", 1); s.insertRule("@property --q { syntax: '<number>'; inherits: true; initial-value: 1 }", 2); return [...s.cssRules].map(x => x.constructor.name).join(","); } catch (e) { return e.name } })());
"##;

const ATRULES_CHROME: &[&str] = &[
    "cqc=rgb(9, 9, 9)",
    "cqx=rgb(0, 0, 0)",
    "named=rgb(19, 19, 19)",
    "sty=rgb(29, 29, 29)",
    "cqw_width=150px",
    "cq_type=inline-size",
    "cqn_name=card",
    "scp=rgb(8, 8, 8)",
    "sc_bg=rgb(1, 1, 1)",
    "sc2p=rgb(18, 18, 18)",
    "stopp=rgb(0, 0, 0)",
    "outp=rgb(0, 0, 0)",
    "px_width=7px",
    "px_var=7px",
    "pem_var=20px",
    "pem_width=20px",
    "pbad_var=7px",
    "pbad_width=7px",
    "pcol_var=rgb(0, 255, 0)",
    "pinh_x=7px",
    "pinh_inh=5px",
    "body_inh=3px",
    "any=",
    "ctors=CSSPropertyRule,CSSPropertyRule,CSSPropertyRule,CSSPropertyRule,CSSStyleRule,CSSStyleRule,CSSContainerRule,CSSContainerRule,CSSContainerRule,CSSContainerRule,CSSStyleRule,CSSScopeRule,CSSScopeRule,CSSPageRule,CSSCounterStyleRule,CSSStyleRule,CSSStyleRule,CSSStyleRule,CSSStyleRule,CSSStyleRule",
    "types=0,0,0,0,1,1,0,0,0,0,1,0,0,6,11,1,1,1,1,1",
    "prop=--x|<length>|false|7px|@property --x { syntax: \"<length>\"; inherits: false; initial-value: 7px; }",
    "anyprop=|@property --any { syntax: \"*\"; inherits: false; }",
    "cont=(min-width: 200px)||(min-width: 200px)|1|@container (min-width: 200px) {\n  .cqc { color: rgb(9, 9, 9); }\n}",
    "cont_named=card (width < 200px)|card|(width < 200px)",
    "scope=.sc||2|@scope (.sc) {\n  p { color: rgb(8, 8, 8); }\n  :scope { background-color: rgb(1, 1, 1); }\n}",
    "scope2=.sc2|.stop|@scope (.sc2) to (.stop) {\n  p { color: rgb(18, 18, 18); }\n}",
    "page=:first|1in|@page :first { margin: 1in; }",
    "cstyle=thumbs|cyclic|\"*\"|\" \"|@counter-style thumbs { system: cyclic; symbols: \"*\"; suffix: \" \"; }",
    "ins=CSSContainerRule,CSSScopeRule,CSSPropertyRule",
];

const HIDDEN: &str = r##"<!doctype html>
<meta charset="utf-8">
<style>
body { font: 16px/18px Arial; margin: 8px }
#sc1 { width: 200px; height: 100px; overflow: scroll }
#sc1::-webkit-scrollbar { width: 8px }
#sc2 { width: 200px; height: 100px; overflow: scroll }
#sc2::-webkit-scrollbar { height: 6px }
#sc3 { width: 200px; height: 100px; overflow: scroll; scrollbar-width: thin }
#sc3::-webkit-scrollbar { width: 8px; height: 8px }
#sc4 { width: 200px; height: 100px; overflow: auto }
#sc4::-webkit-scrollbar { width: 8px; height: 8px }
#sc5 { width: 200px; height: 100px; overflow: scroll }
#sc5::-webkit-scrollbar { display: none }
#sc6 { width: 200px; height: 100px; overflow: scroll }
#sc6::-webkit-scrollbar { width: 0 }
.ctr { counter-reset: n 4 }
.ctr li { counter-increment: n }
.ctr li::before { content: counter(n) ". "; }
#im::before { content: "XXXXXXXX" }
</style>
<body>
<div id=sc1><div style="height:300px;width:300px"></div></div>
<div id=sc2><div style="height:300px;width:300px"></div></div>
<div id=sc3><div style="height:300px;width:300px"></div></div>
<div id=sc4><div style="height:300px;width:300px"></div></div>
<div id=sc5><div style="height:300px;width:300px"></div></div>
<div id=sc6><div style="height:300px;width:300px"></div></div>
<ol class=ctr id=ol><li id=li1>a</li><li id=li2>b</li></ol>
<img id=im style="width:20px;height:20px">
<noscript id=ns><p id=nsp>no script text</p></noscript>
<div id=hd hidden>hidden text</div>
<details id=dt><summary id=sm>sum</summary><div id=dtd style="width:100px;height:100px"></div><p id=dtp>details body</p></details>
<details id=dto open><summary>sum2</summary><p id=dtop>open body</p></details>
<div id=hd2 hidden style="display:block">shown hidden</div>
</body>"##;

const HIDDEN_PROBE: &str = r##"
const q = (id) => document.getElementById(id);
const out = (k, v) => print(k + "=" + v);
for (const i of [1, 2, 3, 4, 5, 6]) { const e = q("sc" + i); out("sc" + i, e.clientWidth + "x" + e.clientHeight); }
out("li_before_content", getComputedStyle(q("li1"), "::before").content);
out("ol_height", q("ol").offsetHeight);
out("im", q("im").getBoundingClientRect().width + " " + getComputedStyle(q("im"), "::before").content);
out("qs_horizontal", (() => { try { return document.querySelectorAll(":horizontal").length } catch (e) { return e.name } })());
out("qs_wkh", (() => { try { return document.querySelectorAll("::-webkit-scrollbar:horizontal").length } catch (e) { return e.name } })());
out("prop_bad", [...document.styleSheets[0].cssRules].filter((x) => x.constructor.name === "CSSPropertyRule").length);
out("ns", getComputedStyle(q("ns")).display + " | " + (q("nsp") ? "nsp" : "no-nsp") + " | h=" + q("ns").offsetHeight + " | " + JSON.stringify(q("ns").innerText) + " | children=" + q("ns").childNodes.length);
out("hd", getComputedStyle(q("hd")).display + " h=" + q("hd").offsetHeight + " " + JSON.stringify(q("hd").innerText));
out("hd2", getComputedStyle(q("hd2")).display + " h=" + q("hd2").offsetHeight + " " + JSON.stringify(q("hd2").innerText));
// `getBoundingClientRect` FIRST: Chrome lays out a closed <details>' content for the rect APIs, and an
// `offsetHeight` read before any of them answers 0 there (the content is skipped until something forces
// it). Laid out, the <p> is a full line: WPT's closed-details-layout-apis asserts the same shape.
out("dt", getComputedStyle(q("dtp")).display + " rect_h=" + q("dtp").getBoundingClientRect().height + " h=" + q("dt").offsetHeight + " " + JSON.stringify(q("dt").innerText) + " dtpIT=" + JSON.stringify(q("dtp").innerText) + " sm=" + getComputedStyle(q("sm")).display);
out("dtd", q("dtd").getBoundingClientRect().width + "x" + q("dtd").getBoundingClientRect().height + " under_summary=" + (q("dtd").getBoundingClientRect().top === q("sm").getBoundingClientRect().bottom));
out("dto", getComputedStyle(q("dtop")).display + " h=" + q("dto").offsetHeight + " " + JSON.stringify(q("dto").innerText));
out("dt_cv", getComputedStyle(q("dt"), "::details-content").display + " / " + getComputedStyle(q("dt"), "::details-content").contentVisibility + " / " + getComputedStyle(q("dto"), "::details-content").contentVisibility);
out("body_text_has_hidden", /no script|hidden text|details body/.test(document.body.innerText));
"##;

const HIDDEN_CHROME: &[&str] = &[
    "sc1=192x85",
    "sc2=185x94",
    "sc3=190x90",
    "sc4=192x92",
    "sc5=200x100",
    "sc6=200x85",
    "li_before_content=counter(n) \". \"",
    "ol_height=36",
    "im=20 \"XXXXXXXX\"",
    "qs_horizontal=0",
    "qs_wkh=0",
    "prop_bad=0",
    "ns=inline | no-nsp | h=0 | \"<p id=nsp>no script text</p>\" | children=1",
    "hd=none h=0 \"hidden text\"",
    "hd2=block h=18 \"shown hidden\"",
    "dt=block rect_h=18 h=18 \"sum\" dtpIT=\"\" sm=list-item",
    "dtd=100x100 under_summary=true",
    "dto=block h=52 \"sum2\\n\\nopen body\"",
    "dt_cv=block / hidden / visible",
    "body_text_has_hidden=false",
];

/// One body under each prolog. Chrome's `compatMode` per prolog, and the quirks it applies. `cdesc` and
/// `cchild` are matched through an ancestor whose class or id differs in case, which the folded ancestor
/// filter answers in a quirks document (Chrome 150, headless, measured 2026-09-30).
const QUIRKS_BODY: &str = r##"<html><head><meta charset="utf-8"><style>
body { font-family: Arial }
.tbl { font-size: 30px; font-weight: bold }
#pct { height: 50% ; width: 10px; background: red }
#unitless { width: 100; height: 20 }
#hash { color: ff0000 }
.CaseCls { color: rgb(3, 3, 3) }
.CaseWrap .inner { color: rgb(4, 4, 4) }
#CaseTop > .deep b { color: rgb(5, 5, 5) }
</style></head>
<body><p id=first>first para</p>
<div class=tbl><table><tr><td id=td>cell</td></tr></table></div>
<div id=imgline><img id=img style="width:10px;height:10px;display:inline"></div>
<div id=pct></div>
<div id=unitless></div>
<div id=hash>h</div>
<div class=casecls id=case>c</div>
<div class=CASEwrap><span class=INNER id=cdesc>d</span></div>
<div id=caseTOP><p class=DEEP><b id=cchild>b</b></p></div>
</body></html>"##;

const QUIRKS_PROBE: &str = r##"
const q = (id) => document.getElementById(id);
const out = (k, v) => print(k + "=" + v);
out("compat", document.compatMode);
out("body_top", document.body.getBoundingClientRect().top);
out("first_top", q("first").getBoundingClientRect().top);
out("first_margin", getComputedStyle(q("first")).marginTop);
out("td_font", getComputedStyle(q("td")).fontSize + " " + getComputedStyle(q("td")).fontWeight);
out("imgline_h", q("imgline").offsetHeight);
out("html_fills", document.documentElement.offsetHeight === innerHeight);
out("body_fills", document.body.offsetHeight === innerHeight - 16);
out("pct_h", q("pct").offsetHeight === Math.round((innerHeight - 16) / 2) ? "half" : q("pct").offsetHeight);
out("unitless_h", q("unitless").offsetHeight);
out("hash", getComputedStyle(q("hash")).color);
out("case", getComputedStyle(q("case")).color);
out("qs_case", document.querySelectorAll(".CaseCls").length);
out("case_desc", getComputedStyle(q("cdesc")).color);
out("case_child", getComputedStyle(q("cchild")).color);
"##;

/// `(prolog, Chrome's answers)`.
const QUIRKS_CHROME: &[(&str, &[&str])] = &[
    (
        "",
        &["compat=BackCompat", "body_top=8", "first_top=8", "first_margin=16px", "td_font=16px 400", "imgline_h=10",
          "html_fills=true", "body_fills=true", "pct_h=half", "unitless_h=20", "hash=rgb(255, 0, 0)", "case=rgb(3, 3, 3)", "qs_case=1",
          "case_desc=rgb(4, 4, 4)", "case_child=rgb(5, 5, 5)"],
    ),
    (
        "<!DOCTYPE HTML PUBLIC \"-//W3C//DTD HTML 4.01 Transitional//EN\">\n",
        &["compat=BackCompat", "body_top=8", "first_top=8", "first_margin=16px", "td_font=16px 400", "imgline_h=10",
          "html_fills=true", "body_fills=true", "pct_h=half", "unitless_h=20", "hash=rgb(255, 0, 0)", "case=rgb(3, 3, 3)", "qs_case=1",
          "case_desc=rgb(4, 4, 4)", "case_child=rgb(5, 5, 5)"],
    ),
    (
        "<!DOCTYPE html SYSTEM \"http://www.ibm.com/data/dtd/v11/ibmxhtml1-transitional.dtd\">\n",
        &["compat=BackCompat", "body_top=8", "first_top=8", "first_margin=16px", "td_font=16px 400", "imgline_h=10",
          "html_fills=true", "body_fills=true", "pct_h=half", "unitless_h=20", "hash=rgb(255, 0, 0)", "case=rgb(3, 3, 3)", "qs_case=1",
          "case_desc=rgb(4, 4, 4)", "case_child=rgb(5, 5, 5)"],
    ),
    (
        "<!DOCTYPE HTML PUBLIC \"-//W3C//DTD HTML 4.01 Transitional//EN\" \"http://www.w3.org/TR/html4/loose.dtd\">\n",
        &["compat=CSS1Compat", "body_top=16", "first_top=16", "first_margin=16px", "td_font=30px 700", "imgline_h=10",
          "html_fills=false", "body_fills=false", "pct_h=0", "unitless_h=0", "hash=rgb(0, 0, 0)", "case=rgb(0, 0, 0)", "qs_case=0",
          "case_desc=rgb(0, 0, 0)", "case_child=rgb(0, 0, 0)"],
    ),
    (
        "<!doctype html>\n",
        &["compat=CSS1Compat", "body_top=16", "first_top=16", "first_margin=16px", "td_font=30px 700", "imgline_h=18",
          "html_fills=false", "body_fills=false", "pct_h=0", "unitless_h=0", "hash=rgb(0, 0, 0)", "case=rgb(0, 0, 0)", "qs_case=0",
          "case_desc=rgb(0, 0, 0)", "case_child=rgb(0, 0, 0)"],
    ),
];

async fn serve(listener: TcpListener, log: Log) {
    loop {
        let Ok((mut s, _)) = listener.accept().await else { return };
        let log = log.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            let Ok(n) = s.read(&mut buf).await else { return };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            if let Ok(mut l) = log.lock() {
                l.push(path.clone());
            }
            let (ctype, body) = match path.as_str() {
                p if p.ends_with(".png") => ("image/png", "x"),
                p if p.ends_with(".woff2") => ("font/woff2", "x"),
                _ => ("text/plain", "no"),
            };
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: {ctype}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(body.as_bytes()).await;
            let _ = s.flush().await;
        });
    }
}

fn bundles() -> Vec<String> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/korvus-persona/bundles");
    let mut out: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("manifest.toml").is_file())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    out.sort();
    assert!(!out.is_empty(), "no persona bundle in the tree");
    out
}

/// Load `doc` at a loopback URL under `bundle` and run `probe`, returning its lines and the access log.
async fn run(bundle: Option<&str>, doc: &str, probe: &str) -> (Vec<String>, Vec<String>) {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener, log.clone()));
    let persona = match bundle {
        Some(id) => persona::CapturedPersona::select(Some(id)).unwrap_or_else(|e| panic!("select {id}: {e}")),
        None => persona::CapturedPersona::default(),
    };
    let mut e = Engine::new(persona::worn_on_host(persona));
    e.run().await.expect("engine boot");
    e.load_page(doc, &format!("http://{addr}/page.html"), &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(8)).await;
    e.eval("p11c-probe", probe.to_string()).expect("eval");
    e.run().await.expect("run");
    let out = e.take_output();
    let mut got = log.lock().unwrap().clone();
    got.sort();
    got.dedup();
    (out, got)
}

#[tokio::test]
async fn pseudo_elements_answer_chromes_way_under_every_bundle_and_load_what_chrome_loads() {
    for id in bundles() {
        let (out, log) = run(Some(&id), PSEUDO, PSEUDO_PROBE).await;
        assert_eq!(out, PSEUDO_CHROME, "wearing {id}");
        // `load_page` is handed the document, so the page itself is not in this log. Chrome's log is
        // this plus the page: `/favicon.ico` (fetched since P10c), the three pseudo-element loads, and
        // NOT the `display: none` element's `::before` or a `::before` with no `content`.
        assert_eq!(log, vec!["/favicon.ico", "/pseudo-bg.png", "/pseudo-content.png", "/pseudo-font.woff2"], "the access log, wearing {id}");
    }
}

#[tokio::test]
async fn container_scope_property_and_page_rules_answer_chromes_way() {
    let (out, log) = run(None, ATRULES, ATRULES_PROBE).await;
    assert_eq!(out, ATRULES_CHROME);
    assert_eq!(log, vec!["/favicon.ico"], "nothing but the favicon to load, as in Chrome");
}

#[tokio::test]
async fn noscript_hidden_and_closed_details_are_not_shown_and_webkit_scrollbars_size_per_axis() {
    for id in bundles() {
        let (out, _) = run(Some(&id), HIDDEN, HIDDEN_PROBE).await;
        // `sc3` is `scrollbar-width: thin`, whose thickness is the worn theme's: 10 px on Linux/Windows,
        // 11 on macOS (its capture), so the thin box is 1 px narrower there.
        let expect: Vec<String> = HIDDEN_CHROME
            .iter()
            .map(|&r| if r == "sc3=190x90" && id.contains("macos") { "sc3=189x89".into() } else { r.into() })
            .collect();
        assert_eq!(out, expect, "wearing {id}");
    }
}

#[tokio::test]
async fn the_doctype_decides_the_mode_and_quirks_mode_lays_out_as_chrome_does() {
    for (prolog, chrome) in QUIRKS_CHROME {
        let doc = format!("{prolog}{QUIRKS_BODY}");
        let (out, _) = run(None, &doc, QUIRKS_PROBE).await;
        assert_eq!(out, *chrome, "prolog {prolog:?}");
    }
}
