//! LIVE gate for the 2D canvas family as four `interface!` tables.
//!
//! Before this lane `core/html/canvas_2d.rs` fetched four prototypes out of the generated shell
//! registry (`shell_proto(scope, "CanvasRenderingContext2D")`) and defined 73 + 1 + 1 + 11 members
//! onto them by hand — a second install mechanism beside `install/installer.rs` (invariant I2), and
//! members the install ledger never saw. The four interfaces are tables now and the installer owns
//! their prototypes.
//!
//! A table cutover has four failure modes, and this file is one section per mode, read off the LIVE
//! surface rather than asserted about the source:
//!
//!   1. **An instance on the prototype.** `CanvasRenderingContext2D.prototype` must not be a live
//!      context — its own keys must be exactly the capture's members plus `constructor`, in the
//!      capture's order (which is also the enumeration-order check the surface finalize pass used to
//!      do for these four and no longer reaches).
//!   2. **A row accepting a foreign receiver.** `ctx.canvas`'s getter refuses one, exactly as the
//!      shell stub it replaces did.
//!   3. **A body reachable where it was inert, or the reverse.** The two deliberately inert members
//!      (`drawFocusIfNeeded`, `isContextLost`) still answer what they answered.
//!   4. **An install ORDER change.** The context object is still one per canvas, still carries its
//!      `canvas` back-reference, and the gradient / pattern / `Path2D` objects it hands back still
//!      wear the right prototypes.

use engine::Engine;

const PAGE: &str = r#"<!doctype html><html><body><canvas id="c" width="40" height="30"></canvas></body></html>"#;

async fn page() -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(PAGE).unwrap();
    e.run().await.unwrap();
    e
}

fn bundle() -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(persona::shipped_artifact("surface")).unwrap()).unwrap()
}

fn str_of(e: &mut Engine, name: &'static str, src: String) -> String {
    e.eval_value(name, src).unwrap().as_str().unwrap().to_string()
}

/// `expr` as "ok:<value>" or "throw:<Ctor>:<message>" — a thrown error and a returned value are both
/// reportable, so a member that stops throwing reads as a diff instead of a panic.
fn probe(e: &mut Engine, expr: &str) -> String {
    let src = format!(
        r#"(() => {{ try {{ const v = ({expr}); return "ok:" + String(v); }}
                     catch (err) {{ return "throw:" + err.constructor.name + ":" + err.message; }} }})()"#
    );
    str_of(e, "l17-probe", src)
}

/// The capture's recorded prototype member names for `iface`, in the capture's order.
fn recorded(fx: &serde_json::Value, iface: &str) -> Vec<String> {
    fx["interfaces"][iface]["members"]
        .as_array()
        .unwrap_or_else(|| panic!("{iface} is absent from the capture"))
        .iter()
        .map(|m| m["name"].as_str().unwrap().to_string())
        .collect()
}

// -- 1. no instance on the prototype, and the capture's order ------------------------------------

/// Every own key of each table's prototype, against the BUNDLE: the same names, in the same order,
/// plus `constructor`. This is the assertion a table cutover exists to make — the installer built
/// these prototypes from the rows, and the rows were derived from the capture — and it is also the
/// one that catches an instance leaking onto the class template, because a live 2D context's state
/// would appear here as extra own keys.
#[tokio::test]
async fn every_prototype_carries_exactly_the_captured_members_in_order() {
    let mut e = page().await;
    let fx = bundle();
    for iface in ["CanvasRenderingContext2D", "CanvasGradient", "CanvasPattern", "Path2D"] {
        let want = recorded(&fx, iface);
        let src = format!(r#"Object.getOwnPropertyNames({iface}.prototype).filter(n => n !== "constructor").join(",")"#);
        let got = str_of(&mut e, "l17-order", src);
        assert_eq!(
            got,
            want.join(","),
            "{iface}.prototype is not the capture's member list in the capture's order"
        );
        // `constructor` is present, non-enumerable, and points back at the interface object.
        let src = format!(
            r#"(() => {{ const d = Object.getOwnPropertyDescriptor({iface}.prototype, "constructor");
                         return [d.value === {iface}, d.enumerable, d.writable, d.configurable].join("|"); }})()"#
        );
        assert_eq!(str_of(&mut e, "l17-ctor", src), "true|false|true|true", "{iface}.prototype.constructor");
        // ...and the `Symbol.toStringTag` the table declares.
        assert_eq!(
            probe(&mut e, &format!(r#"{iface}.prototype[Symbol.toStringTag]"#)),
            format!("ok:{iface}"),
            "{iface}.prototype[Symbol.toStringTag]"
        );
    }
}

/// `new X()` on the three non-newable interfaces throws, as the capture records (`"newable": false`);
/// `Path2D` really constructs, because its table is built through `build_newable` over a real body.
#[tokio::test]
async fn only_path2d_is_newable() {
    let mut e = page().await;
    for iface in ["CanvasRenderingContext2D", "CanvasGradient", "CanvasPattern"] {
        let got = probe(&mut e, &format!("new {iface}()"));
        assert!(got.starts_with("throw:TypeError:"), "new {iface}() answered {got}");
    }
    assert_eq!(probe(&mut e, "Object.prototype.toString.call(new Path2D())"), "ok:[object Path2D]");
    assert_eq!(probe(&mut e, "Object.getPrototypeOf(new Path2D()) === Path2D.prototype"), "ok:true");
    // A `Path2D` instance has no own properties: its segment list is a v8 private.
    assert_eq!(probe(&mut e, "Object.getOwnPropertyNames(new Path2D()).length"), "ok:0");
    // `new Path2D(other)` copies the donor's segments, which is the reason the ctor has a body at all.
    let src = r#"(() => { const a = new Path2D(); a.rect(0, 0, 4, 4);
                          const b = new Path2D(a); b.moveTo(1, 1); return typeof b; })()"#;
    assert_eq!(str_of(&mut e, "l17-p2d-copy", src.to_string()), "object");
}

// -- 2. the foreign receiver ---------------------------------------------------------------------

/// `ctx.canvas` is the one row of this family with a receiver check, and it has to KEEP it: the
/// generated shell's readonly-attribute stub checked the receiver against the prototype it was
/// mounted on, and a hand table's plain `= stub` row would NOT have (the D2 stub takes no receiver).
/// So the row is implemented rather than stubbed — a native getter over the private slot
/// `getContext` writes — and a foreign receiver is `Illegal invocation`, which is Chrome's answer.
#[tokio::test]
async fn the_canvas_back_reference_refuses_a_foreign_receiver() {
    let mut e = page().await;
    assert_eq!(
        probe(&mut e, r#"Object.getOwnPropertyDescriptor(CanvasRenderingContext2D.prototype, "canvas").get.call({})"#),
        "throw:TypeError:Illegal invocation",
    );
    // ...and it answers for a real context.
    assert_eq!(probe(&mut e, r#"document.getElementById("c").getContext("2d").canvas.tagName"#), "ok:CANVAS");
    assert_eq!(
        probe(&mut e, r#"(() => { const c = document.getElementById("c"); return c.getContext("2d").canvas === c; })()"#),
        "ok:true",
    );
    // The back-reference is a PRIVATE slot now, not an own data property shadowing a stub, so a 2D
    // context has no own properties at all — which is what Chrome's has.
    assert_eq!(
        probe(&mut e, r#"Object.getOwnPropertyNames(document.getElementById("c").getContext("2d")).length"#),
        "ok:0",
    );
}

/// The WebIDL arity guard each method row carries. Before the cutover these were wrapped by the
/// registry-wide `install_arity_guards` pass; a table row is wrapped by `live::rows::method_row` at
/// install time instead, from the SAME `required` count, so the message must be identical — and a
/// member WebIDL requires nothing of must still not be wrapped.
#[tokio::test]
async fn every_row_keeps_its_recorded_arity_guard() {
    let mut e = page().await;
    for (iface, expr, member, need) in [
        ("CanvasRenderingContext2D", r#"document.getElementById("c").getContext("2d").fillRect()"#, "fillRect", 4),
        ("CanvasRenderingContext2D", r#"document.getElementById("c").getContext("2d").measureText()"#, "measureText", 1),
        ("Path2D", "new Path2D().moveTo()", "moveTo", 2),
        ("CanvasGradient", r#"document.getElementById("c").getContext("2d").createLinearGradient(0,0,1,1).addColorStop()"#, "addColorStop", 2),
    ] {
        assert_eq!(
            probe(&mut e, expr),
            format!("throw:TypeError:Failed to execute '{member}' on '{iface}': {need} argument{} required, but only 0 present.", if need == 1 { "" } else { "s" }),
            "{iface}.{member} lost its arity guard"
        );
    }
    // `required 0` members are NOT wrapped: an extra guard breaks pages, which is the worse half.
    assert_eq!(probe(&mut e, r#"document.getElementById("c").getContext("2d").save()"#), "ok:undefined");
    assert_eq!(probe(&mut e, r#"document.getElementById("c").getContext("2d").getLineDash().length"#), "ok:0");
    assert_eq!(probe(&mut e, "new Path2D().closePath()"), "ok:undefined");
    assert_eq!(
        probe(&mut e, r#"document.getElementById("c").getContext("2d").createPattern(document.getElementById("c"), "repeat") === null"#),
        "ok:false",
    );
}

// -- 3. the members that are deliberately inert --------------------------------------------------

/// Two members of this family answer without doing anything, on purpose, and both must stay that
/// way: a table cutover that made one reachable (or silenced a real one) is a defect in either
/// direction. `drawFocusIfNeeded` draws no ring because nothing a script just called is focused, and
/// `isContextLost` is false because a CPU raster surface has no device to lose. Neither is a stub
/// row — both are implemented bodies whose honest answer is "nothing happened".
#[tokio::test]
async fn the_deliberately_inert_members_are_still_inert() {
    let mut e = page().await;
    assert_eq!(probe(&mut e, r#"document.getElementById("c").getContext("2d").isContextLost()"#), "ok:false");
    assert_eq!(
        probe(&mut e, r#"document.getElementById("c").getContext("2d").drawFocusIfNeeded(document.body)"#),
        "ok:undefined",
    );
    // ...and the argument check is the observable half of `drawFocusIfNeeded`.
    assert_eq!(
        probe(&mut e, r#"document.getElementById("c").getContext("2d").drawFocusIfNeeded()"#),
        "throw:TypeError:Failed to execute 'drawFocusIfNeeded' on 'CanvasRenderingContext2D': 1 argument required, but only 0 present.",
    );
}

// -- 4. the install ORDER: the context is still one object, and still draws ----------------------

/// `getContext` is defined by the same op that now installs these four tables, and the context it
/// hands back has to be the SAME object every call, wear the table's prototype, and still record.
#[tokio::test]
async fn the_context_object_is_one_object_and_still_records() {
    let mut e = page().await;
    let src = r#"(() => {
        const c = document.getElementById("c");
        const a = c.getContext("2d"), b = c.getContext("2d");
        a.fillStyle = "red";
        a.fillRect(0, 0, 10, 10);
        return [a === b,
                a instanceof CanvasRenderingContext2D,
                Object.prototype.toString.call(a),
                b.fillStyle,
                a.lineWidth].join("|");
    })()"#;
    assert_eq!(
        str_of(&mut e, "l17-identity", src.to_string()),
        "true|true|[object CanvasRenderingContext2D]|#ff0000|1",
    );
}

/// The three objects the context mints wear the TABLES' prototypes — the mint sites used to read
/// `shell_proto`, and read `install::live::set_table_proto` now. A gradient whose prototype is not
/// the table's would have no `addColorStop` at all.
#[tokio::test]
async fn the_minted_paint_objects_wear_the_table_prototypes() {
    let mut e = page().await;
    let src = r#"(() => {
        const ctx = document.getElementById("c").getContext("2d");
        const g = ctx.createLinearGradient(0, 0, 10, 10);
        g.addColorStop(0, "red");
        const p = ctx.createPattern(document.getElementById("c"), "repeat");
        return [g instanceof CanvasGradient,
                Object.prototype.toString.call(g),
                Object.getOwnPropertyNames(g).length,
                p instanceof CanvasPattern,
                Object.prototype.toString.call(p),
                typeof ctx.measureText("hi").width].join("|");
    })()"#;
    assert_eq!(
        str_of(&mut e, "l17-paints", src.to_string()),
        "true|[object CanvasGradient]|0|true|[object CanvasPattern]|number",
    );
}

/// Lane L18's `OffscreenCanvasRenderingContext2D` borrow reads THESE prototypes, from a pass that
/// runs after this install. It is the one consumer whose correctness depends on where this lane's
/// install sits in the order, so it is asserted here too.
#[tokio::test]
async fn the_offscreen_borrow_still_finds_the_donor() {
    let mut e = page().await;
    let src = r#"(() => {
        const o = new OffscreenCanvas(8, 8).getContext("2d");
        o.lineWidth = 7;
        return [Object.prototype.toString.call(o.measureText("hi")),
                Object.prototype.toString.call(o.createLinearGradient(0, 0, 1, 1)),
                o.lineWidth].join("|");
    })()"#;
    assert_eq!(
        str_of(&mut e, "l17-offscreen", src.to_string()),
        "[object TextMetrics]|[object CanvasGradient]|7",
    );
}
