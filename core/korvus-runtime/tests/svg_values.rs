//! The SVG animated-attribute value types, as REGISTRY rows (`kits::svg_animated` / `kits::svg_value`,
//! bodies in `core/korvus-webapi/src/core/dom/svg_values.rs`) rather than the JS builders behind
//! `KB.__svgAnimated` that a second installer mounted.
//!
//! EVERY expected value below is Chrome 150 headless (`--dump-dom` over a loopback server) on the same
//! markup -- including the four the JS had wrong, which is why they are here:
//!
//! * `SVG_ANGLETYPE_RAD` is 3 and `GRAD` is 4 (the JS had them swapped);
//! * `2em` is 32 user units (the JS had no `em` factor);
//! * an absent length's `valueAsString` is `"0"` (the JS gave `""`);
//! * `x.baseVal !== x.animVal` (the JS returned one object for both).
//!
//! And the SHAPE, which is what this lane exists for: Chrome's `rect.x` and `rect.x.baseVal` have NO
//! own properties -- their members are real accessors on their prototypes. Ours had thirteen.
use engine::Engine;

async fn probe(html: &str, script: &str) -> Vec<String> {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(html).unwrap();
    e.eval("a", script.into()).unwrap();
    e.run().await.unwrap();
    e.take_output()
}

const PAGE: &str = r#"<!doctype html><svg id="s"><rect id="r" x="10" y="3mm" width="50%" height="2em"/>
<text id="t" x="1 2mm 3"/><g id="g" transform="rotate(30 1 2) skewX(9)"/>
<marker id="mk" orient="45grad"/><a id="an"/>
<linearGradient id="lg"/>
<svg id="inner" viewBox="1 2 3 4" preserveAspectRatio="xMinYMax slice"/></svg>"#;

/// THE SHAPE. `Object.getOwnPropertyNames` of an animated value and of its `baseVal` is empty in
/// Chrome, because the members are prototype accessors. Every own property here used to be a
/// structural divergence the grader counts.
#[tokio::test]
async fn animated_values_have_no_own_properties() {
    let out = probe(
        PAGE,
        r#"
        const r = document.getElementById("r"), inner = document.getElementById("inner");
        print("x=" + JSON.stringify(Object.getOwnPropertyNames(r.x)));
        print("x.baseVal=" + JSON.stringify(Object.getOwnPropertyNames(r.x.baseVal)));
        print("viewBox=" + JSON.stringify(Object.getOwnPropertyNames(inner.viewBox)));
        print("viewBox.baseVal=" + JSON.stringify(Object.getOwnPropertyNames(inner.viewBox.baseVal)));
        print("par.baseVal=" + JSON.stringify(Object.getOwnPropertyNames(inner.preserveAspectRatio.baseVal)));
        print("tag=" + Object.prototype.toString.call(r.x));
        print("tagBase=" + Object.prototype.toString.call(r.x.baseVal));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "x=[]",
            "x.baseVal=[]",
            "viewBox=[]",
            "viewBox.baseVal=[]",
            "par.baseVal=[]",
            "tag=[object SVGAnimatedLength]",
            "tagBase=[object SVGLength]",
        ]
    );
}

/// `[SameObject]` holds per (element, member) AND per half: Chrome hands back the same `baseVal`
/// every read and a DIFFERENT object for `animVal`.
#[tokio::test]
async fn animated_values_are_same_object_per_half() {
    let out = probe(
        PAGE,
        r#"
        const r = document.getElementById("r");
        print("x=" + (r.x === r.x));
        print("base=" + (r.x.baseVal === r.x.baseVal));
        print("anim=" + (r.x.animVal === r.x.animVal));
        print("halves=" + (r.x.baseVal === r.x.animVal));
        print("live=" + (r.setAttribute("x", "42"), r.x.baseVal.value));
        "#,
    )
    .await;
    assert_eq!(out, vec!["x=true", "base=true", "anim=true", "halves=false", "live=42"]);
}

/// The four length readings, with the unit table Chrome resolves them through. `3mm` and `2em` are
/// the two the old JS got wrong (`em` had no factor at all).
#[tokio::test]
async fn lengths_read_their_units() {
    let out = probe(
        PAGE,
        r#"
        const r = document.getElementById("r");
        const row = (n) => { const b = r[n].baseVal;
          return n + "=" + [b.unitType, b.value.toFixed(4), b.valueInSpecifiedUnits, b.valueAsString].join("|"); };
        print(row("x"));
        print(row("y"));
        print(row("height"));
        print(row("rx"));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "x=1|10.0000|10|10",
            "y=7|11.3386|3|3mm",
            "height=3|32.0000|2|2em",
            // An ABSENT length is the initial value: user-unit zero, whose string is "0", not "".
            "rx=1|0.0000|0|0",
        ]
    );
}

/// `SVG_ANGLETYPE_*` is UNSPECIFIED 1, DEG 2, RAD 3, GRAD 4 -- the capture's own constants. The JS
/// this replaces had RAD and GRAD swapped, so `45grad` reported unit 3 and `1rad` unit 4.
#[tokio::test]
async fn angles_use_the_declared_unit_numbering() {
    let out = probe(
        PAGE,
        r#"
        const mk = document.getElementById("mk");
        const row = () => { const b = mk.orientAngle.baseVal;
          return [b.unitType, b.value.toFixed(4), b.valueInSpecifiedUnits, b.valueAsString].join("|"); };
        print("grad=" + row());
        mk.setAttribute("orient", "1rad");
        print("rad=" + row());
        mk.setAttribute("orient", "30");
        print("bare=" + row());
        print("consts=" + [SVGAngle.SVG_ANGLETYPE_DEG, SVGAngle.SVG_ANGLETYPE_RAD, SVGAngle.SVG_ANGLETYPE_GRAD].join(","));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec!["grad=4|40.5000|45|45grad", "rad=3|57.2958|1|1rad", "bare=1|30.0000|30|30", "consts=2,3,4"]
    );
}

/// A list is a LIVE view over the attribute, each item a value type of its own with its own unit.
/// `getItem` out of range is an `IndexSizeError`; with no argument it is a WebIDL arity `TypeError`.
#[tokio::test]
async fn lists_read_through_to_the_attribute() {
    let out = probe(
        PAGE,
        r#"
        const t = document.getElementById("t"), b = t.x.baseVal;
        print("n=" + [b.length, b.numberOfItems].join(","));
        print("items=" + [0,1,2].map(i => [b.getItem(i).valueAsString, b.getItem(i).unitType].join(":")).join(" "));
        print("itemOwn=" + JSON.stringify(Object.getOwnPropertyNames(b.getItem(0))));
        print("fresh=" + (b.getItem(0) === b.getItem(0)));
        try { b.getItem(9); print("oob=none"); } catch (e) { print("oob=" + e.name); }
        // WebIDL `unsigned long` is ToUint32: -100 asks for index 4294967196, not index 0
        // (svg/types/scripted/SVGLengthList-getItem.html).
        try { b.getItem(-100); print("neg=none"); } catch (e) { print("neg=" + e.name); }
        try { b.getItem(); print("arity=none"); } catch (e) { print("arity=" + e.name); }
        t.setAttribute("x", "7");
        print("live=" + [b.numberOfItems, b.getItem(0).value].join(","));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "n=3,3",
            "items=1:1 2mm:7 3:1",
            "itemOwn=[]",
            "fresh=false",
            "oob=IndexSizeError",
            "neg=IndexSizeError",
            "arity=TypeError",
            "live=1,7",
        ]
    );
}

/// SVG's "number-optional-number" and "integer-optional-integer" attributes put TWO numbers in one
/// attribute and expose them as two `SVGAnimated*` members, so the member reading the first is handed
/// the whole attribute: a JS `parseFloat` prefix parse, not a strict one. Chrome 150 measured on the
/// same markup; also `svg/animations/svg{number,integer}optional*-animation-invalid-value-*.html`.
#[tokio::test]
async fn optional_second_number_attributes_prefix_parse() {
    let out = probe(
        r#"<!doctype html><svg><filter><feGaussianBlur id="b" stdDeviation="5 3"/>
        <feGaussianBlur id="one" stdDeviation="5"/><feGaussianBlur id="none"/>
        <feConvolveMatrix id="c" order="3 4"/></filter></svg>"#,
        r#"
        const b = document.getElementById("b"), c = document.getElementById("c"),
              one = document.getElementById("one"), none = document.getElementById("none");
        print("pair=" + [b.stdDeviationX.baseVal, b.stdDeviationY.baseVal].join(","));
        print("int=" + [c.orderX.baseVal, c.orderY.baseVal].join(","));
        print("single=" + [one.stdDeviationX.baseVal, one.stdDeviationY.baseVal].join(","));
        print("absent=" + [none.stdDeviationX.baseVal, none.stdDeviationY.baseVal].join(","));
        b.setAttribute("stdDeviation", "junk");
        print("junk=" + [b.stdDeviationX.baseVal, b.stdDeviationY.baseVal].join(","));
        b.setAttribute("stdDeviation", "7,2");
        print("comma=" + [b.stdDeviationX.baseVal, b.stdDeviationY.baseVal].join(","));
        b.stdDeviationX.baseVal = 9;
        print("setX=" + [b.getAttribute("stdDeviation"), b.stdDeviationX.baseVal, b.stdDeviationY.baseVal].join(","));
        one.stdDeviationY.baseVal = 8;
        print("setY=" + [one.getAttribute("stdDeviation"), one.stdDeviationX.baseVal, one.stdDeviationY.baseVal].join(","));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "pair=5,3",
            "int=3,4",
            "single=5,5",
            "absent=0,0",
            "junk=0,0",
            "comma=7,2",
            "setX=9 2,9,2",
            "setY=5 8,5,8",
        ]
    );
}

/// A transform list parses `translate(...) rotate(...)`; each item's `type` is the `SVG_TRANSFORM_*`
/// enumeration and its `angle` is the rotation/skew angle in degrees.
#[tokio::test]
async fn transform_lists_parse_their_functions() {
    let out = probe(
        PAGE,
        r#"
        const b = document.getElementById("g").transform.baseVal;
        print("n=" + b.numberOfItems);
        print("i0=" + [b.getItem(0).type, b.getItem(0).angle].join(","));
        print("i1=" + [b.getItem(1).type, b.getItem(1).angle].join(","));
        print("own=" + JSON.stringify(Object.getOwnPropertyNames(b.getItem(0))));
        "#,
    )
    .await;
    assert_eq!(out, vec!["n=2", "i0=4,30", "i1=5,9", "own=[]"]);
}

/// `viewBox` is four numbers; `preserveAspectRatio` is the align keyword plus meet/slice, both as the
/// `SVG_PRESERVEASPECTRATIO_*` / `SVG_MEETORSLICE_*` enumerations.
#[tokio::test]
async fn rect_and_preserve_aspect_ratio_parse_their_attributes() {
    let out = probe(
        PAGE,
        r#"
        const inner = document.getElementById("inner"), v = inner.viewBox.baseVal;
        print("viewBox=" + [v.x, v.y, v.width, v.height].join(","));
        const p = inner.preserveAspectRatio.baseVal;
        print("par=" + [p.align, p.meetOrSlice].join(","));
        "#,
    )
    .await;
    assert_eq!(out, vec!["viewBox=1,2,3,4", "par=8,2"]);
}

/// A scalar `SVGAnimated*` carries the attribute itself, and its `baseVal` is WRITABLE: Chrome writes
/// the content attribute through (`a.href.baseVal = "zz"` leaves `href="zz"`).
#[tokio::test]
async fn scalar_animated_values_round_trip_through_the_attribute() {
    let out = probe(
        PAGE,
        r#"
        const an = document.getElementById("an"), lg = document.getElementById("lg");
        print("href0=" + JSON.stringify(an.href.baseVal));
        an.href.baseVal = "zz";
        print("href1=" + [an.getAttribute("href"), an.href.baseVal, an.href.animVal].join(","));
        print("target=" + Object.prototype.toString.call(an.target));
        lg.setAttribute("gradientUnits", "userSpaceOnUse");
        print("enum=" + [lg.gradientUnits.baseVal, lg.gradientUnits.animVal].join(","));
        print("enumOwn=" + JSON.stringify(Object.getOwnPropertyNames(lg.gradientUnits)));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "href0=\"\"",
            "href1=zz,zz,zz",
            "target=[object SVGAnimatedString]",
            "enum=1,1",
            "enumOwn=[]",
        ]
    );
}

/// A LIVE value's setter writes the content attribute back, keeping the unit it found.
#[tokio::test]
async fn live_value_setters_write_the_attribute() {
    let out = probe(
        PAGE,
        r#"
        const r = document.getElementById("r");
        r.x.baseVal.value = 20;
        print("x=" + [r.getAttribute("x"), r.x.baseVal.value].join(","));
        r.y.baseVal.valueInSpecifiedUnits = 5;
        print("y=" + [r.getAttribute("y"), r.y.baseVal.unitType].join(","));
        const inner = document.getElementById("inner");
        inner.viewBox.baseVal.width = 9;
        print("viewBox=" + inner.getAttribute("viewBox"));
        "#,
    )
    .await;
    assert_eq!(out, vec!["x=20,20", "y=5mm,7", "viewBox=1 2 9 4"]);
}

/// A DETACHED value (`svg.createSVGLength()`) starts at its IDL initial value, has no own properties,
/// and round-trips a write -- which is what the shell's slot pair used to give it, kept.
#[tokio::test]
async fn detached_values_start_at_the_idl_initial_value() {
    let out = probe(
        PAGE,
        r#"
        const s = document.getElementById("s");
        const l = s.createSVGLength();
        print("len=" + [l.unitType, l.value, l.valueInSpecifiedUnits, l.valueAsString].join(","));
        print("own=" + JSON.stringify(Object.getOwnPropertyNames(l)));
        l.value = 5;
        print("len2=" + [l.unitType, l.value, l.valueAsString].join(","));
        const a = s.createSVGAngle();
        print("angle=" + [a.unitType, a.value, a.valueAsString].join(","));
        const n = s.createSVGNumber(); n.value = 7;
        print("num=" + n.value);
        const rc = s.createSVGRect();
        print("rect=" + [rc.x, rc.y, rc.width, rc.height].join(","));
        const tr = s.createSVGTransform();
        print("transform=" + [tr.type, tr.angle].join(","));
        print("tags=" + [l, a, n, rc, tr].map(o => Object.prototype.toString.call(o)).join(" "));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "len=1,0,0,0",
            "own=[]",
            "len2=1,5,5",
            "angle=1,0,0",
            "num=7",
            "rect=0,0,0,0",
            "transform=1,0",
            "tags=[object SVGLength] [object SVGAngle] [object SVGNumber] [object SVGRect] [object SVGTransform]",
        ]
    );
}

/// A value-type accessor called on something that is not one is Chrome's `TypeError: Illegal
/// invocation`, not a silent `undefined`. And a minted value object carries the SHELL BRAND, so a
/// member of its interface reached through it passes the receiver check -- `clear`, and `consolidate`, which
/// answers the list's one consolidated transform as Chrome 150's does since it has a body
/// (the stub answered null). The first version of this lane minted unbranded objects and
/// `scripts/coverage.sh` measured three members losing `verified`; this is the regression gate for it.
#[tokio::test]
async fn value_members_check_their_receiver() {
    let out = probe(
        PAGE,
        r#"
        const g = (I, n) => Object.getOwnPropertyDescriptor(globalThis[I].prototype, n).get;
        for (const [I, n] of [["SVGLength", "value"], ["SVGAnimatedLength", "baseVal"], ["SVGRect", "x"]]) {
          try { g(I, n).call({}); print(I + "." + n + "=none"); }
          catch (e) { print(I + "." + n + "=" + e.name); }
        }
        try { SVGLengthList.prototype.getItem.call({}, 0); print("getItem=none"); }
        catch (e) { print("getItem=" + e.name); }
        print("clear=" + document.getElementById("t").x.baseVal.clear());
        print("consolidate=" + document.getElementById("g").transform.baseVal.consolidate());
        // webref says `optional DOMMatrix2DInit matrix = {}`; Chrome 150 measured over loopback says
        // "1 argument required, but only 0 present". Reachable only now that createSVGTransform()
        // answers an object -- while it answered undefined the case passed on undefined.setMatrix().
        const tr = document.getElementById("s").createSVGTransform();
        try { tr.setMatrix(); print("setMatrix=none"); } catch (e) { print("setMatrix=" + e.name); }
        print("setMatrix.length=" + SVGTransform.prototype.setMatrix.length);
        print("native=" + /\[native code\]/.test(String(g("SVGLength", "value"))));
        print("name=" + g("SVGLength", "value").name);
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "SVGLength.value=TypeError",
            "SVGAnimatedLength.baseVal=TypeError",
            "SVGRect.x=TypeError",
            "getItem=TypeError",
            "clear=undefined",
            "consolidate=[object SVGTransform]",
            "setMatrix=TypeError",
            "setMatrix.length=1",
            "native=true",
            "name=get value",
        ]
    );
}
