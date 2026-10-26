//! Stylesheets that apply when Chrome's don't, the form state selectors read, and
//! `document.title`, in a live engine over a loopback server whose access log is the truth.
//!
//! # The oracle
//!
//! Every expected value below is stock Google Chrome 150.0.7871.46's (`--headless=new`, 780x600
//! window, 2026-09-18), read over CDP by running the SAME probe (with `print` collecting lines) over the
//! SAME markup, served by a loopback server that logged every request -- the `*_CHROME` arrays are that
//! output, copied verbatim. The L19 rows these came from:
//!
//! ```text
//! E5   <style media=print> / <link media=print> applied to the screen      gnu.org, fsf.org, cdc.gov, ...
//! E6   CSS-wide `revert` computed as `unset`, so `all:unset;display:revert`
//!      made every <script>/<style> visible text                            toss.im, css-tricks.com
//! E8   <link rel="preload stylesheet"> never fetched, never applied         vuejs.org, vitejs.dev, ...
//! E16  :checked read the attribute, not the checkedness script set          kubernetes.io
//! E17  document.title not stripped/collapsed; an SVG <title> taken          ziglang.org, docs.npmjs.com, ...
//! ```

use engine::Engine;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

type Log = Arc<Mutex<Vec<String>>>;

const TITLE: &str = r##"<!doctype html><html><head><title>  a
 b	c  d </title></head><body><svg><title>body svg</title></svg><math><title>body math</title></math>x</body></html>"##;

const TITLE_PROBE: &str = r##"
const out = (k, v) => print(k + "=" + v);
const J = JSON.stringify;
out("initial", J(document.title));
out("text", J(document.querySelector("title").text));
const svgIn = new DOMParser().parseFromString("<!doctype html><body><svg><title>svg only</title></svg><math><title>mt</title></math>", "text/html");
out("svg_math_only", J(svgIn.title));
const both = new DOMParser().parseFromString("<!doctype html><body><svg><title>svg one</title></svg><title>html  two</title>", "text/html");
out("html_after_svg", J(both.title));
const t = document.querySelector("title");
t.appendChild(document.createElement("b")).textContent = "BOLD";
t.appendChild(document.createTextNode(" C "));
out("child_text", J(document.title) + " " + J(t.text));
t.text = "z";
out("text_set", t.childNodes.length + " " + J(document.title));
document.title = "";
out("empty", J(document.title) + " " + t.childNodes.length);
t.remove();
document.title = "  set  me\n now ";
const made = document.head.lastElementChild;
out("created", J(document.title) + " " + made.nodeName + " " + made.namespaceURI + " " + J(made.textContent) + " " + document.querySelectorAll("title").length);
const sv = new DOMParser().parseFromString('<svg xmlns="http://www.w3.org/2000/svg"><g><title>nested</title></g><title>  root  child\n t </title></svg>', "image/svg+xml");
out("svg_doc", J(sv.title));
sv.title = "  svg  set ";
out("svg_doc_set", J(sv.title) + " " + sv.documentElement.firstChild.nodeName);
const sv2 = new DOMParser().parseFromString('<svg xmlns="http://www.w3.org/2000/svg"><g><title>nested</title></g></svg>', "image/svg+xml");
out("svg_doc_nested_only", J(sv2.title));
sv2.title = "made";
const f = sv2.documentElement.firstChild;
out("svg_doc_made", J(sv2.title) + " " + f.nodeName + " " + f.namespaceURI + " " + sv2.getElementsByTagName("title").length);
const cd = document.implementation.createDocument("http://www.w3.org/2000/svg", "svg", null);
cd.title = "x  y";
out("create_svg", J(cd.title) + " " + cd.documentElement.childNodes.length);
out("create_html", J(document.implementation.createHTMLDocument("  hh  ii ").title));
const xd = document.implementation.createDocument(null, "root", null);
xd.title = "q";
out("create_xml", J(xd.title) + " " + xd.documentElement.childNodes.length);
const obs = new MutationObserver(() => {});
obs.observe(document.head, { childList: true, subtree: true });
document.title = "observed";
out("observed", obs.takeRecords().map((r) => r.type + ":" + r.target.nodeName + "+" + r.addedNodes.length + "-" + r.removedNodes.length).join(","));
document.documentElement.removeChild(document.head);
document.title = "nohead";
out("no_head", J(document.title) + " " + document.querySelectorAll("title").length);
"##;

/// Chrome 150's answers to [`TITLE_PROBE`] over [`TITLE`].
const TITLE_CHROME: &[&str] = &[
    "initial=\"a b c d\"",
    "text=\"  a\\n b\\tc  d \"",
    "svg_math_only=\"\"",
    "html_after_svg=\"html two\"",
    "child_text=\"a b c d C\" \"  a\\n b\\tc  d  C \"",
    "text_set=1 \"z\"",
    "empty=\"\" 0",
    "created=\"set me now\" TITLE http://www.w3.org/1999/xhtml \"  set  me\\n now \" 3",
    "svg_doc=\"root child t\"",
    "svg_doc_set=\"svg set\" g",
    "svg_doc_nested_only=\"\"",
    "svg_doc_made=\"made\" title http://www.w3.org/2000/svg 2",
    "create_svg=\"x y\" 1",
    "create_html=\"hh ii\"",
    "create_xml=\"\" 0",
    "observed=childList:TITLE+1-1",
    "no_head=\"\" 2",
];

const MEDIA: &str = r##"<!doctype html><html><head>
<link id=lp rel=stylesheet href=print.css media=print>
<link rel=stylesheet href=screen.css media="screen and (min-width: 10px)">
<link rel=stylesheet href=narrow.css media="(max-width: 10px)">
<style id=sp media=print>.p2 { display: none }</style>
<style>@media print { .p3 { display: none } }</style>
<style media="">.p4 { display: none }</style>
<style>@import url(imp-print.css) print; @import url(imp-screen.css) screen;</style>
<style media="garbage((">.p9 { display: none }</style>
<style media="print, screen">.p10 { display: none }</style>
<style media="SCREEN">.p11 { display: none }</style>
<style media="not print">.p12 { display: none }</style>
</head><body><p class=p1>p1</p><p class=p2>p2</p><p class=p3>p3</p><p class=p4>p4</p><p class=p5>p5</p><p class=p6>p6</p><p class=p7>p7</p><p class=p8>p8</p><p class=p9>p9</p><p class=p10>p10</p><p class=p11>p11</p><p class=p12>p12</p>
</body></html>"##;

const MEDIA_PROBE: &str = r##"
const out = (k, v) => print(k + "=" + v);
const J = JSON.stringify;
const text = () => J(document.body.innerText);
const s = document.getElementById("sp");
const l = document.getElementById("lp");
out("loaded", text());
out("sheets", document.styleSheets.length + " " + [...document.styleSheets].map((x) => x.media.mediaText).join("|"));
out("rules_readable", s.sheet.cssRules.length + " " + l.sheet.cssRules.length);
out("match_media", matchMedia("print").matches + " " + matchMedia(s.media).matches + " " + matchMedia("screen").matches);
out("p2_display", getComputedStyle(document.querySelector(".p2")).display);
s.media = "screen";
out("style_media_screen", text() + " " + s.sheet.media.mediaText);
l.media = "all";
out("link_media_all", text());
s.removeAttribute("media");
out("style_media_removed", text() + " " + J(s.sheet.media.mediaText) + " " + s.sheet.media.length);
l.setAttribute("media", "print");
out("link_media_print", text());
s.sheet.media.mediaText = "print";
out("sheet_media_text_print", text() + " " + J(s.getAttribute("media")));
s.setAttribute("media", "all");
out("attribute_wins_again", text() + " " + s.sheet.media.mediaText);
s.sheet.media.appendMedium("print");
out("append_medium", text() + " " + J(s.getAttribute("media")) + " " + J(s.sheet.media.mediaText));
s.sheet.media.deleteMedium("all");
out("delete_medium", text() + " " + J(s.sheet.media.mediaText));
const n = document.createElement("style");
n.media = "print";
n.textContent = ".p4 { color: rgb(1, 2, 3) } .p5 { display: none }";
document.head.appendChild(n);
out("inserted_print", getComputedStyle(document.querySelector(".p4")).color + " " + text());
n.media = "(min-width: 1px)";
out("inserted_min_width", getComputedStyle(document.querySelector(".p4")).color);
const c = new CSSStyleSheet({ media: "print" });
c.replaceSync(".p1 { color: rgb(4, 5, 6) }");
document.adoptedStyleSheets = [c];
out("adopted_print", getComputedStyle(document.querySelector(".p1")).color);
c.media.mediaText = "all";
out("adopted_all", getComputedStyle(document.querySelector(".p1")).color);
const texts = ["SCREEN", "Screen And (MIN-WIDTH:10PX)", "garbage((", "print,screen", "  print  ", "(min-width:  100px)", "not PRINT", "only screen and (max-width: 50em)", "foo", "(bogus: 1)", "print, garbage((", "ALL"];
for (const t of texts) {
  const s = document.createElement("style");
  s.media = t;
  document.head.appendChild(s);
  const c = new CSSStyleSheet({ media: t });
  print(JSON.stringify(t) + " => " + JSON.stringify(s.sheet.media.mediaText) + " " + s.sheet.media.length + " " + JSON.stringify(c.media.mediaText) + " " + JSON.stringify(s.sheet.media.item(0)));
}
"##;

/// Chrome 150's answers to [`MEDIA_PROBE`] over [`MEDIA`].
const MEDIA_CHROME: &[&str] = &[
    "loaded=\"p1\\n\\np2\\n\\np3\\n\\np6\\n\\np8\\n\\np9\"",
    "sheets=11 print|screen and (min-width: 10px)|(max-width: 10px)|print||||garbage((|print, screen|screen|not print",
    "rules_readable=1 1",
    "match_media=false false true",
    "p2_display=block",
    "style_media_screen=\"p1\\n\\np3\\n\\np6\\n\\np8\\n\\np9\" screen",
    "link_media_all=\"p3\\n\\np6\\n\\np8\\n\\np9\"",
    "style_media_removed=\"p3\\n\\np6\\n\\np8\\n\\np9\" \"\" 0",
    "link_media_print=\"p1\\n\\np3\\n\\np6\\n\\np8\\n\\np9\"",
    "sheet_media_text_print=\"p1\\n\\np2\\n\\np3\\n\\np6\\n\\np8\\n\\np9\" null",
    "attribute_wins_again=\"p1\\n\\np3\\n\\np6\\n\\np8\\n\\np9\" all",
    "append_medium=\"p1\\n\\np3\\n\\np6\\n\\np8\\n\\np9\" \"all\" \"all, print\"",
    "delete_medium=\"p1\\n\\np2\\n\\np3\\n\\np6\\n\\np8\\n\\np9\" \"print\"",
    "inserted_print=rgb(0, 0, 0) \"p1\\n\\np2\\n\\np3\\n\\np6\\n\\np8\\n\\np9\"",
    "inserted_min_width=rgb(1, 2, 3)",
    "adopted_print=rgb(0, 0, 0)",
    "adopted_all=rgb(4, 5, 6)",
    "\"SCREEN\" => \"screen\" 1 \"screen\" \"screen\"",
    "\"Screen And (MIN-WIDTH:10PX)\" => \"screen and (min-width: 10px)\" 1 \"screen and (min-width: 10px)\" \"screen and (min-width: 10px)\"",
    "\"garbage((\" => \"garbage((\" 1 \"garbage((\" \"garbage((\"",
    "\"print,screen\" => \"print, screen\" 2 \"print, screen\" \"print\"",
    "\"  print  \" => \"print\" 1 \"print\" \"print\"",
    "\"(min-width:  100px)\" => \"(min-width: 100px)\" 1 \"(min-width: 100px)\" \"(min-width: 100px)\"",
    "\"not PRINT\" => \"not print\" 1 \"not print\" \"not print\"",
    "\"only screen and (max-width: 50em)\" => \"only screen and (max-width: 50em)\" 1 \"only screen and (max-width: 50em)\" \"only screen and (max-width: 50em)\"",
    "\"foo\" => \"foo\" 1 \"foo\" \"foo\"",
    "\"(bogus: 1)\" => \"(bogus: 1)\" 1 \"(bogus: 1)\" \"(bogus: 1)\"",
    "\"print, garbage((\" => \"print, garbage((\" 2 \"print, garbage((\" \"print\"",
    "\"ALL\" => \"all\" 1 \"all\" \"all\"",
];

const REVERT: &str = r##"<!doctype html><html><head><style>
@layer reset { :where(:not(html, iframe, canvas, img, svg, video, audio):not(svg *, symbol *)) { all: unset; display: revert } }
#sp { color: revert } #wrap { color: rgb(1, 2, 3) } #h1 { font-size: revert }
#mu { margin: unset } #mi { margin: initial } #mr { margin: revert }
@layer one { #rl { color: rgb(10, 0, 0) } } @layer two { #rl { color: revert-layer } }
@layer one { #rl2 { color: rgb(20, 0, 0) } } #rl2 { color: revert-layer }
#rl3 { color: rgb(30, 0, 0) } #rl3 { color: revert-layer }
#ar { all: revert } #ar { margin-top: 3px }
#ci { color: initial } #cu { color: unset }
#bu { display: inline } #bu { display: revert }
#li { display: revert }
#imp { display: revert !important } #imp { display: inline }
#var { --d: revert; display: var(--d) }
@layer one { #rlu { color: rgb(40, 0, 0) } } #rlu { color: rgb(50, 0, 0) } @layer two { #rlu { color: revert-layer } }
@layer a { #chain { color: rgb(70, 0, 0) } } @layer b { #chain { color: revert-layer } } @layer c { #chain { color: revert-layer } }
#dispunset { display: unset }
#b { font-weight: revert }
#tbl { display: revert }
#mbs { margin-block-start: revert }
#pad { padding-left: 7px } #pad { padding-inline-start: revert }
.q p { color: rgb(9, 9, 9) }
</style></head><body><div id=wrap>
<p id=p>para one</p><script id=sc>var x = 1</script><div id=dv>block div</div><a id=a href="#x">lnk</a><span id=sp>sp</span><h1 id=h1>h</h1>
<p id=mu>mu</p><p id=mi>mi</p><p id=mr>mr</p><span id=rl>rl</span><span id=rl2>rl2</span><span id=rl3>rl3</span><p id=ar>ar</p><style id=st></style>
<span id=ci>ci</span><span id=cu>cu</span><button id=bu>bu</button><ul><li id=li>li</li></ul><div id=imp>imp</div><div id=var>var</div><span id=rlu>rlu</span><span id=chain>chain</span>
<div id=dispunset>du</div><b id=b>b</b><table id=tbl><tr><td>t</td></tr></table><p id=mbs>mbs</p><ul id=pad><li>x</li></ul>
<noscript>ns</noscript><template>tp</template><select><option>o1</option></select>
</div>
<div class=q><p id=ia style="display:revert">a</p><p id=ib style="color:revert">b</p><p id=ic style="color:revert-layer">c</p><span id=id style="display:revert-layer">d</span></div>
</body></html>"##;

const REVERT_PROBE: &str = r##"
const q = (id) => document.getElementById(id);
const g = (id, p) => getComputedStyle(q(id))[p];
const out = (k, v) => print(k + "=" + v);
for (const [id, p] of [["p", "display"], ["sc", "display"], ["dv", "display"], ["a", "color"], ["sp", "color"], ["h1", "fontSize"], ["mu", "marginTop"], ["mi", "marginTop"], ["mr", "marginTop"], ["mr", "marginBottom"], ["rl", "color"], ["rl2", "color"], ["rl3", "color"], ["ar", "display"], ["ar", "marginTop"], ["ar", "marginBottom"], ["st", "display"], ["ci", "color"], ["cu", "color"], ["bu", "display"], ["li", "display"], ["imp", "display"], ["var", "display"], ["rlu", "color"], ["chain", "color"], ["dispunset", "display"], ["b", "fontWeight"], ["tbl", "display"], ["mbs", "marginTop"], ["pad", "paddingLeft"], ["ia", "display"], ["ib", "color"], ["ic", "color"], ["id", "display"]]) {
  out(id + "." + p, g(id, p));
}
out("text", JSON.stringify(q("wrap").innerText));
out("inline_values", JSON.stringify(q("ia").style.display) + " " + JSON.stringify(q("ic").style.color) + " " + q("ic").style.getPropertyValue("color"));
const rule = [...document.styleSheets[0].cssRules].find((r) => r.selectorText === "#rl2");
out("rule_css", rule.cssText + " | " + rule.style.getPropertyValue("color"));
q("p").style.display = "revert-layer";
out("set_revert_layer", JSON.stringify(q("p").style.display) + " " + g("p", "display"));
"##;

/// Chrome 150's answers to [`REVERT_PROBE`] over [`REVERT`].
const REVERT_CHROME: &[&str] = &[
    "p.display=block",
    "sc.display=none",
    "dv.display=block",
    "a.color=rgb(1, 2, 3)",
    "sp.color=rgb(1, 2, 3)",
    "h1.fontSize=32px",
    "mu.marginTop=0px",
    "mi.marginTop=0px",
    "mr.marginTop=16px",
    "mr.marginBottom=16px",
    "rl.color=rgb(10, 0, 0)",
    "rl2.color=rgb(20, 0, 0)",
    "rl3.color=rgb(1, 2, 3)",
    "ar.display=block",
    "ar.marginTop=3px",
    "ar.marginBottom=16px",
    "st.display=none",
    "ci.color=rgb(0, 0, 0)",
    "cu.color=rgb(1, 2, 3)",
    "bu.display=inline-block",
    "li.display=list-item",
    "imp.display=block",
    "var.display=inline",
    "rlu.color=rgb(50, 0, 0)",
    "chain.color=rgb(70, 0, 0)",
    "dispunset.display=inline",
    "b.fontWeight=700",
    "tbl.display=table",
    "mbs.marginTop=16px",
    "pad.paddingLeft=40px",
    "ia.display=block",
    "ib.color=rgb(0, 0, 0)",
    "ic.color=rgb(9, 9, 9)",
    "id.display=inline",
    "text=\"para one\\n\\nblock div\\nlnksp\\nh\\n\\nmu\\n\\nmi\\n\\nmr\\n\\nrlrl2rl3\\n\\nar\\n\\ncicubu\\nli\\nimp\\nvarrluchain dub\\nt\\n\\nmbs\\n\\nx\\no1\"",
    "inline_values=\"revert\" \"revert-layer\" revert-layer",
    "rule_css=#rl2 { color: revert-layer; } | revert-layer",
    "set_revert_layer=\"revert-layer\" block",
];

const FORMS: &str = r##"<!doctype html><html><head><style>
nav ul.foldable { display: none } nav input:checked ~ ul.foldable { display: block }
#t:checked + label { color: rgb(0, 128, 0) } input:indeterminate + i { color: rgb(1, 1, 1) }
input:placeholder-shown + b { color: rgb(2, 2, 2) } input:invalid + u { color: rgb(3, 3, 3) }
option:checked { color: rgb(4, 4, 4) } form:invalid { color: rgb(5, 5, 5) } fieldset:disabled { color: rgb(6, 6, 6) }
</style></head><body>
<nav><input type=checkbox id=cb><label for=cb>lbl</label><ul class=foldable><li>folded item</li></ul></nav>
<input type=checkbox id=t><label id=tl>tl</label><input type=checkbox id=ii><i id=i>i</i><input id=pp placeholder=x><b id=bb>b</b><input id=rq required><u id=uu>u</u>
<form id=f>
<input type=checkbox id=cb1><input type=checkbox id=cb2 checked><input type=checkbox id=cb3><input type=checkbox id=cb4 checked>
<input type=radio name=g id=r1 checked><input type=radio name=g id=r2><input type=radio name=h id=h1><input type=radio name=h id=h2>
<input type=radio name=dup id=d1 checked><input type=radio name=dup id=d2 checked>
<select id=sel><option id=o1>1</option><option id=o2 selected>2</option><option id=o3>3</option></select>
<select id=sel2><option id=q1>1</option><option id=q2>2</option></select>
<select id=sel3><option id=n1 disabled>1</option><option id=n2>2</option></select>
<select id=selm multiple><option id=m1>1</option><option id=m2 selected>2</option><option id=m3 selected>3</option></select>
<select id=selr required><option id=rp value="">pick</option><option id=rv>v</option></select>
<optgroup id=og disabled><option id=ogo>x</option></optgroup>
<input id=ph placeholder=hello><input id=ph2 placeholder=hello><textarea id=ta placeholder=tt></textarea><input id=ph3 placeholder=hi value=x><input id=phe placeholder=""><textarea id=ta2 placeholder=tt>content</textarea><input type=number id=phn placeholder=n><input type=checkbox id=phc placeholder=c>
<input id=req required><input id=req2 required><input id=em type=email value="bad"><input id=num type=number min=1 max=5 value=3>
<input id=url type=url value="nope"><input id=pat pattern="[0-9]{3}" value="12a"><input id=pat2 pattern="[0-9]{3}" value="123"><input id=step type=number step=2 value=3>
<input type=checkbox id=reqcb required><input type=radio name=rg id=rg1 required><input type=radio name=rg id=rg2>
<input id=ro required readonly><input id=hid type=hidden required><input id=cust>
<button id=btn1>b1</button><button id=btn2>b2</button><input type=submit id=sub><button id=rst type=reset>r</button>
<fieldset id=fs disabled><legend id=lg><input id=inleg></legend><input id=infs><legend><input id=inleg2></legend><fieldset id=fsin><input id=deep></fieldset></fieldset>
<fieldset id=fsok><input id=fsreq required></fieldset>
<input id=dis><progress id=pr></progress><progress id=pr2 value=1></progress>
<input type=checkbox id=ind>
<datalist><input id=indl required></datalist>
</form><form id=f2><input id=f2in required></form><form id=f3><input id=f3in></form>
<input id=outside form=f3 required>
<input type=radio name=g id=rx checked>
</body></html>"##;

const FORMS_PROBE: &str = r##"
const $ = (id) => document.getElementById(id);
const m = (id, s) => ($(id).matches(s) ? 1 : 0);
const c = (id) => getComputedStyle($(id)).color;
const out = (k, v) => print(k + "=" + v);
const row = (name, sel, ids) => out(name, ids.map((i) => i + m(i, sel)).join(","));
$("cb").checked = true;
out("text", JSON.stringify(document.querySelector("nav").innerText));
out("before", [c("tl"), c("i"), c("bb"), c("uu")].join(" "));
row("checked0", ":checked", ["cb2", "r1", "rx", "d1", "d2", "o1", "o2", "q1", "q2", "n1", "n2", "m1", "m2", "m3", "rp"]);
row("default0", ":default", ["cb2", "r1", "o2", "q1", "n1", "m2", "btn1", "btn2", "sub", "rst"]);
out("idl0", [$("r1").checked, $("rx").checked, $("d1").checked, $("d2").checked, $("q1").selected, $("sel2").selectedIndex, $("n2").selected, $("sel3").selectedIndex].join(","));
$("t").checked = true; $("ii").indeterminate = true; $("pp").value = "v"; $("rq").value = "v";
out("after", [c("tl"), c("i"), c("bb"), c("uu")].join(" "));
$("cb").checked = false;
out("text2", JSON.stringify(document.querySelector("nav").innerText));
$("cb1").checked = true; $("cb2").checked = false; $("cb3").setAttribute("checked", ""); $("cb4").checked = true; $("cb4").removeAttribute("checked");
$("r2").checked = true; $("o3").selected = true; $("sel2").selectedIndex = 1;
$("ph2").value = "typed"; $("req2").value = "x"; $("ind").indeterminate = true; $("dis").disabled = true; $("ta").value = "";
$("cust").setCustomValidity("nope");
row("checked", ":checked", ["cb1", "cb2", "cb3", "cb4", "r1", "r2", "rx", "h1", "o1", "o2", "o3", "q1", "q2"]);
out("idl", [$("r1").checked, $("rx").checked, $("o2").selected, $("sel").selectedIndex, $("cb4").checked].join(","));
row("default", ":default", ["cb1", "cb2", "cb3", "cb4", "r1", "r2", "o2", "o3", "q1", "btn1", "btn2", "sub", "f2in"]);
out("option_color", [c("o2"), c("o3")].join(" "));
row("indeterminate", ":indeterminate", ["cb1", "ind", "r1", "h1", "h2", "rg1", "pr", "pr2"]);
row("disabled", ":disabled", ["infs", "inleg", "inleg2", "deep", "fsin", "dis", "fs", "lg", "o1", "sel", "og", "ogo", "n1"]);
row("enabled", ":enabled", ["infs", "inleg", "inleg2", "deep", "fsin", "dis", "fs", "lg", "o1", "sel", "og", "ogo", "n1", "btn1", "pr", "f"]);
out("fs_color", c("fs"));
row("placeholder", ":placeholder-shown", ["ph", "ph2", "ta", "ph3", "phe", "ta2", "phn", "phc"]);
row("valid", ":valid", ["req", "req2", "em", "num", "url", "pat", "pat2", "step", "reqcb", "rg1", "rg2", "ro", "hid", "cust", "btn1", "rst", "infs", "indl", "f", "f2", "f3", "fs", "fsok", "ph", "dis", "sel", "selr", "rp", "pr", "outside"]);
row("invalid", ":invalid", ["req", "req2", "em", "num", "url", "pat", "pat2", "step", "reqcb", "rg1", "rg2", "ro", "hid", "cust", "btn1", "rst", "infs", "indl", "f", "f2", "f3", "fs", "fsok", "ph", "dis", "sel", "selr", "rp", "pr", "outside"]);
out("form_color", [c("f"), c("f3")].join(" "));
$("num").value = "9"; $("rg2").checked = true; $("reqcb").checked = true; $("selr").selectedIndex = 1;
row("later", ":invalid", ["num", "rg1", "rg2", "reqcb", "selr"]);
row("range", ":out-of-range", ["num", "step", "req"]);
row("inrange", ":in-range", ["num", "step", "req"]);
const clone = $("cb1").cloneNode();
out("clone", [clone.checked, clone.matches(":checked")].join(","));
"##;

/// Chrome 150's answers to [`FORMS_PROBE`] over [`FORMS`].
const FORMS_CHROME: &[&str] = &[
    "text=\"lbl\\nfolded item\"",
    "before=rgb(0, 0, 0) rgb(0, 0, 0) rgb(2, 2, 2) rgb(3, 3, 3)",
    "checked0=cb21,r11,rx1,d10,d21,o10,o21,q11,q20,n10,n21,m10,m21,m31,rp1",
    "default0=cb21,r11,o21,q10,n10,m21,btn11,btn20,sub0,rst0",
    "idl0=true,true,false,true,true,0,true,1",
    "after=rgb(0, 128, 0) rgb(1, 1, 1) rgb(0, 0, 0) rgb(0, 0, 0)",
    "text2=\"lbl\"",
    "checked=cb11,cb20,cb31,cb41,r10,r21,rx1,h10,o10,o20,o31,q10,q21",
    "idl=false,true,false,2,true",
    "default=cb10,cb21,cb31,cb40,r11,r20,o21,o30,q10,btn11,btn20,sub0,f2in0",
    "option_color=rgb(0, 0, 0) rgb(4, 4, 4)",
    "indeterminate=cb10,ind1,r10,h11,h21,rg11,pr1,pr20",
    "disabled=infs1,inleg0,inleg21,deep1,fsin1,dis1,fs1,lg0,o10,sel0,og1,ogo1,n11",
    "enabled=infs0,inleg1,inleg20,deep0,fsin0,dis0,fs0,lg0,o11,sel1,og0,ogo0,n10,btn11,pr0,f0",
    "fs_color=rgb(6, 6, 6)",
    "placeholder=ph1,ph20,ta1,ph30,phe1,ta20,phn1,phc0",
    "valid=req0,req21,em0,num1,url0,pat0,pat21,step1,reqcb0,rg10,rg20,ro0,hid0,cust0,btn11,rst0,infs0,indl0,f0,f20,f30,fs1,fsok0,ph1,dis0,sel1,selr0,rp0,pr0,outside0",
    "invalid=req1,req20,em1,num0,url1,pat1,pat20,step0,reqcb1,rg11,rg21,ro0,hid0,cust1,btn10,rst0,infs0,indl0,f1,f21,f31,fs0,fsok1,ph0,dis0,sel0,selr1,rp0,pr0,outside1",
    "form_color=rgb(5, 5, 5) rgb(5, 5, 5)",
    "later=num1,rg10,rg20,reqcb0,selr0",
    "range=num1,step0,req0",
    "inrange=num0,step0,req0",
    "clone=true,true",
];

const LINKS: &str = r##"<!doctype html><html><head>
<link rel="preload stylesheet" href=a.css as=style>
<link rel=preload href=b.css as=style onload="this.onload=null;this.rel='stylesheet'">
<link rel=preload href=c.css as=style>
<link id=d rel=preload href=d.css as=style>
<link rel="stylesheet preload" href=e.css>
<link id=f rel=stylesheet href=f.css>
<link rel="alternate stylesheet" href=g.css title=alt>
<link rel="alternate stylesheet" href=h.css>
<link rel=stylesheet href=i.css disabled>
<link rel=" STYLESHEET " href=j.css>
<script>
addEventListener("load", () => setTimeout(() => {
  window.snap = [JSON.stringify(document.body.innerText), document.styleSheets.length,
    [...document.querySelectorAll("link")].map((l) => l.rel + ":" + (l.sheet ? "S" : "-")).join(",")].join(" | ");
  document.getElementById("d").rel = "stylesheet";
  document.getElementById("f").rel = "preload";
}, 200));
</script>
</head><body><p class=pa>pa</p><p class=pb>pb</p><p class=pc>pc</p><p class=pd>pd</p><p class=pe>pe</p><p class=pf>pf</p><p class=pg>pg</p><p class=ph>ph</p><p class=pi>pi</p><p class=pj>pj</p><p>plain</p></body></html>"##;

const LINKS_PROBE: &str = r##"
print("at_load=" + window.snap);
print("after_rel_swap=" + JSON.stringify(document.body.innerText) + " | " + document.styleSheets.length + " | " + [...document.querySelectorAll("link")].map((l) => l.rel + ":" + (l.sheet ? "S" : "-")).join(","));
const st = document.createElement("style"); st.textContent = "p { color: rgb(1, 2, 3) }"; document.head.appendChild(st);
const p = document.createElement("p"); document.body.appendChild(p);
const before = getComputedStyle(p).color; st.remove();
print("removed_style=" + before + " " + getComputedStyle(p).color + " " + document.styleSheets.length);
"##;

/// Chrome 150's answers to [`LINKS_PROBE`] over [`LINKS`].
const LINKS_CHROME: &[&str] = &[
    "at_load=\"pc\\n\\npd\\n\\npg\\n\\nph\\n\\npi\\n\\nplain\" | 7 | preload stylesheet:S,stylesheet:S,preload:-,preload:-,stylesheet preload:S,stylesheet:S,alternate stylesheet:S,alternate stylesheet:S,stylesheet:-, STYLESHEET :S",
    "after_rel_swap=\"pc\\n\\npf\\n\\npg\\n\\nph\\n\\npi\\n\\nplain\" | 7 | preload stylesheet:S,stylesheet:S,preload:-,stylesheet:S,stylesheet preload:S,preload:-,alternate stylesheet:S,alternate stylesheet:S,stylesheet:-, STYLESHEET :S",
    "removed_style=rgb(1, 2, 3) rgb(0, 0, 0) 7",
];

/// The subresources the pages name: `(path, body)`, all served as `text/css`.
const FILES: &[(&str, &str)] = &[
    ("/print.css", ".p1 { display: none }"),
    ("/screen.css", ".p5 { display: none }"),
    ("/narrow.css", ".p8 { display: none }"),
    ("/imp-print.css", ".p6 { display: none }"),
    ("/imp-screen.css", ".p7 { display: none }"),
    ("/a.css", ".pa { display: none }"),
    ("/b.css", ".pb { display: none }"),
    ("/c.css", ".pc { display: none }"),
    ("/d.css", ".pd { display: none }"),
    ("/e.css", ".pe { display: none }"),
    ("/f.css", ".pf { display: none }"),
    ("/g.css", ".pg { display: none }"),
    ("/h.css", ".ph { display: none }"),
    ("/i.css", ".pi { display: none }"),
    ("/j.css", ".pj { display: none }"),
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
            let head = match FILES.iter().find(|(p, _)| *p == path) {
                Some((_, body)) => format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/css\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                ),
                None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
            };
            let _ = s.write_all(head.as_bytes()).await;
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

/// Load `doc` at a loopback URL under `bundle`, let it settle, and run `probe`, returning its lines and
/// the access log (in request order: a sheet fetched twice appears twice).
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
    // What a page's own `load` handler schedules (a `rel` swap 200 ms after load) runs before the probe,
    // as it does in Chrome, where the probe is evaluated 2 s after navigation.
    let _ = e.pump(Duration::from_millis(800)).await;
    e.eval("p12b-probe", probe.to_string()).expect("eval");
    e.run().await.expect("run");
    let out = e.take_output();
    let got = log.lock().unwrap().clone();
    (out, got)
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[tokio::test]
async fn document_title_strips_collapses_and_finds_the_title_element_chromes_way() {
    let (out, _) = run(None, TITLE, TITLE_PROBE).await;
    assert_eq!(out, TITLE_CHROME);
}

#[tokio::test]
async fn revert_and_revert_layer_roll_the_cascade_back_to_the_origin_or_layer_below() {
    let (out, _) = run(None, REVERT, REVERT_PROBE).await;
    assert_eq!(out, REVERT_CHROME);
}

#[tokio::test]
async fn form_pseudo_classes_read_the_state_script_set_and_idl_reads_the_same_state() {
    for id in bundles() {
        let (out, _) = run(Some(&id), FORMS, FORMS_PROBE).await;
        assert_eq!(out, FORMS_CHROME, "wearing {id}");
    }
}

#[tokio::test]
async fn a_links_rel_tokens_decide_its_load_and_a_preload_is_consumed_not_applied() {
    for id in bundles() {
        let (out, log) = run(Some(&id), LINKS, LINKS_PROBE).await;
        assert_eq!(out, LINKS_CHROME, "wearing {id}");
        // Chrome's access log over the same page: every sheet ONCE -- the loadCSS link and the script's
        // `rel = "stylesheet"` consume their preloads, the disabled and alternate links are requested.
        let css: Vec<String> = sorted(log.into_iter().filter(|p| p.ends_with(".css")).collect());
        assert_eq!(
            css,
            vec!["/a.css", "/b.css", "/c.css", "/d.css", "/e.css", "/f.css", "/g.css", "/h.css", "/i.css", "/j.css"],
            "the stylesheet requests, wearing {id}"
        );
    }
}

#[tokio::test]
async fn a_sheets_own_media_list_decides_whether_it_applies_and_the_attribute_only_initialises_it() {
    for id in bundles() {
        let (out, log) = run(Some(&id), MEDIA, MEDIA_PROBE).await;
        assert_eq!(out, MEDIA_CHROME, "wearing {id}");
        // Chrome fetches a `<link media=print>` and an `@import … print` all the same: the list gates
        // the rules, never the request.
        assert_eq!(
            sorted(log),
            vec!["/favicon.ico", "/imp-print.css", "/imp-screen.css", "/narrow.css", "/print.css", "/screen.css"],
            "the access log, wearing {id}"
        );
    }
}
