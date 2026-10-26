//! Behavior gate.
//!
//! P0 — native trusted dispatch: an engine-dispatched mouse event reaches a page listener with
//! `isTrusted===true` and consistent fields (movementX/Y carried through), while a JS-constructed event
//! reports `isTrusted===false`. This asserts the trusted-event FOUNDATION works end to end — nothing
//! behavioral matters until this holds (today the JS Event base hardcoded isTrusted=false).
use engine::Engine;

/// An engine-dispatched mousemove is `isTrusted===true` at a page listener with the fields it was given;
/// a `new MouseEvent()` from page JS is `isTrusted===false`. (The native getter reads a Rust private slot
/// page JS cannot reach or forge.)
#[tokio::test]
async fn p0_engine_dispatch_is_trusted_js_is_not() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_document("<!doctype html><html><body></body></html>")
        .expect("load doc");
    e.eval(
        "p0-listen",
        r#"
        document.addEventListener('mousemove', (ev) => {
            print('MM:' + ev.isTrusted + ',' + ev.type + ',' + ev.clientX + ',' + ev.clientY
                + ',' + ev.movementX + ',' + ev.movementY);
        });
        const j = new MouseEvent('mousemove', { clientX: 1 });
        print('JS:' + j.isTrusted);
        "#
        .to_string(),
    )
    .expect("eval listen");
    e.run().await.expect("run listen");
    // The engine (behavior-layer entry point) dispatches a genuinely-trusted mousemove.
    e.dispatch_trusted_mouse("mousemove", 100.0, 120.0, 5.0, -3.0, 0, 0)
        .expect("dispatch");
    e.run().await.expect("run dispatch");
    let out = e.take_output();
    assert!(
        out.iter().any(|l| l == "JS:false"),
        "JS-constructed event must be untrusted; output: {out:?}"
    );
    assert!(
        out.iter().any(|l| l == "MM:true,mousemove,100,120,5,-3"),
        "engine-dispatched event must be trusted with consistent fields; output: {out:?}"
    );
}

/// P2 — end-to-end: a single neuromotor move drives many genuinely-trusted `mousemove` events into a page
/// listener, each `isTrusted`, with `movementX/Y` == the consecutive `screenX/Y` delta and a monotonic
/// `timeStamp` (the scheduler's virtual clock). This is what a behavior-tier detector actually scores.
#[tokio::test(start_paused = true)]
async fn p2_move_mouse_streams_trusted_human_events() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_document("<!doctype html><html><body></body></html>")
        .expect("load doc");
    e.eval(
        "p2-listen",
        r#"
        globalThis.__mm = 0; globalThis.__pm = 0; globalThis.__trusted = 0; globalThis.__badmove = 0; globalThis.__badts = 0;
        globalThis.__px = null; globalThis.__py = null; globalThis.__lastts = -1;
        document.addEventListener('mousemove', (ev) => {
            globalThis.__mm++;
            if (ev.isTrusted) globalThis.__trusted++;
            if (globalThis.__px !== null &&
                (ev.movementX !== ev.screenX - globalThis.__px || ev.movementY !== ev.screenY - globalThis.__py))
                globalThis.__badmove++;
            globalThis.__px = ev.screenX; globalThis.__py = ev.screenY;
            if (ev.timeStamp < globalThis.__lastts) globalThis.__badts++;
            globalThis.__lastts = ev.timeStamp;
        });
        document.addEventListener('pointermove', (ev) => {
            globalThis.__pm++;
            if (ev.isTrusted) globalThis.__trusted++;
            if (ev.timeStamp < globalThis.__lastts) globalThis.__badts++;
            globalThis.__lastts = ev.timeStamp;
        });
        // A move now also drags a hover chain across the page, so it fires the four
        // boundary events wherever the trajectory crosses an element edge. They are counted here
        // for the same reason the two move streams are -- the assertion below is "every event the
        // engine reports emitting reached a listener", and a listener that ignores a whole family
        // of events stops checking that the moment the family exists.
        globalThis.__hv = 0;
        // FIXER C6: the POINTER boundary family fires with the mouse one, as in Chrome.
        for (const type of ['mouseover', 'mouseout', 'mouseenter', 'mouseleave',
                            'pointerover', 'pointerout', 'pointerenter', 'pointerleave']) {
            document.addEventListener(type, (ev) => {
                globalThis.__hv++;
                if (ev.isTrusted) globalThis.__trusted++;
                if (ev.timeStamp < globalThis.__lastts) globalThis.__badts++;
                globalThis.__lastts = ev.timeStamp;
            }, true);   // capture: mouseenter/mouseleave do not bubble
        }
        "#
        .to_string(),
    )
    .expect("eval listen");
    e.run().await.expect("run listen");

    // From (0,0) to a distant target: the neuromotor generator plans the trajectory, the scheduler
    // dispatches every sample as a trusted event.
    let n = e.move_mouse(520.0, 380.0, 40.0).await.expect("move");
    e.run().await.expect("run move");

    e.eval(
        "p2-report",
        "print('P2:' + __mm + ',' + __pm + ',' + __trusted + ',' + __badmove + ',' + __badts + ',' + __hv);".to_string(),
    )
    .expect("eval report");
    e.run().await.expect("run report");
    let out = e.take_output();
    let line = out
        .iter()
        .find(|l| l.starts_with("P2:"))
        .unwrap_or_else(|| panic!("no P2 report; output: {out:?}"))
        .clone();
    let nums: Vec<i64> = line[3..].split(',').map(|s| s.parse().unwrap()).collect();
    let (mm, pm, trusted, badmove, badts) = (nums[0] as usize, nums[1] as usize, nums[2] as usize, nums[3], nums[4]);
    let hv = nums[5] as usize;

    // FIXER C6: delivered once per display frame (60 Hz), as Chrome delivers a mouse, the samples between frames
    // riding along as coalesced events -- so fewer events than samples, and still a stream, not a teleport.
    assert!(n >= 30, "a real move emits many pointer+mouse events (not a teleport), got {n}");
    assert_eq!(mm, pm, "each sample fires a pointermove AND a compat mousemove (paired)");
    assert_eq!(mm + pm + hv, n, "listener saw {} events, engine emitted {n}", mm + pm + hv);
    assert_eq!(trusted, n, "every dispatched event must be isTrusted ({trusted}/{n})");
    assert_eq!(badmove, 0, "movementX/Y must equal the consecutive screenX/Y delta");
    assert_eq!(badts, 0, "event timeStamps must be monotonic");
}

/// P3 — end-to-end: typing streams genuinely-trusted key events into page listeners, with a keydown
/// per character and monotonic timeStamps (the human dwell/IKI structure is validated offline by the
/// behavior oracle; here we prove the trusted-dispatch wiring).
///
/// A later change CHANGED WHAT THIS COUNTS, and Chrome is why. The listener used to watch `keydown` and
/// `keyup` only, and `kn == n` held because those were the only two events the engine produced.
/// They are not the only two a browser produces: stock Chrome 150, asked with a capturing listener
/// on `window`, fires `keypress` between them for every key that types a character, and this engine
/// now does too. A listener that still watched two of the three would have gone on asserting
/// `kn == n` against a number that no longer meant "every dispatched event reached a listener" — it
/// would have meant "the ones I happen to watch did", which is the check quietly stopping.
///
/// The target moved with it: with nothing focused Chrome puts a key event on `<body>`, not on
/// `document`, so `e.target` is asserted here as well. `<body>` is not an editable control, so no
/// `beforeinput`/`input` follows — `typing_live.rs` is where the editing half is pinned.
#[tokio::test(start_paused = true)]
async fn p3_type_text_streams_trusted_key_events() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_document("<!doctype html><html><body></body></html>")
        .expect("load doc");
    e.eval(
        "p3-listen",
        r#"
        globalThis.__kn = 0; globalThis.__kt = 0; globalThis.__downs = 0;
        globalThis.__press = 0; globalThis.__offbody = 0;
        globalThis.__lastts = -1; globalThis.__badts = 0;
        const rec = (ev) => {
            globalThis.__kn++;
            if (ev.isTrusted) globalThis.__kt++;
            if (ev.type === 'keydown') globalThis.__downs++;
            if (ev.type === 'keypress') globalThis.__press++;
            if (ev.target !== document.body) globalThis.__offbody++;
            if (ev.timeStamp < globalThis.__lastts) globalThis.__badts++;
            globalThis.__lastts = ev.timeStamp;
        };
        document.addEventListener('keydown', rec);
        document.addEventListener('keypress', rec);
        document.addEventListener('keyup', rec);
        "#
        .to_string(),
    )
    .expect("eval listen");
    e.run().await.expect("run listen");

    let n = e.type_text("hello world").await.expect("type");
    e.run().await.expect("run type");

    e.eval(
        "p3-report",
        "print('P3:' + __kn + ',' + __kt + ',' + __downs + ',' + __badts + ',' + __press + ',' + __offbody);".to_string(),
    )
    .expect("eval report");
    e.run().await.expect("run report");
    let out = e.take_output();
    let line = out
        .iter()
        .find(|l| l.starts_with("P3:"))
        .unwrap_or_else(|| panic!("no P3 report; output: {out:?}"))
        .clone();
    let nums: Vec<i64> = line[3..].split(',').map(|s| s.parse().unwrap()).collect();
    let (kn, kt, downs, badts) = (nums[0] as usize, nums[1] as usize, nums[2], nums[3]);
    let (press, offbody) = (nums[4], nums[5]);

    assert_eq!(kn, n, "listener saw {kn} key events, engine emitted {n}");
    assert!(downs >= 11, "expected >= one keydown per char (11), got {downs}");
    assert_eq!(press, downs, "Chrome fires a keypress for every character key; got {press}/{downs}");
    assert_eq!(offbody, 0, "with nothing focused every key event targets <body>");
    assert_eq!(kt, n, "every key event must be isTrusted ({kt}/{n})");
    assert_eq!(badts, 0, "key event timeStamps must be monotonic");
}

/// P4 — end-to-end: a scroll gesture streams genuinely-trusted `wheel` events into a page listener,
/// shaped like the input device THIS PERSONA implies.
///
/// This test read "the persona is macOS -> trackpad" and asserted a trackpad's shape outright: at
/// least twelve events, fractional deltas. Both are true of a trackpad and false of a mouse, so
/// switching the shipped bundle from a Mac capture to a Linux one turned it red -- a 600 px scroll
/// became five clean 120 px notches, which is exactly right and exactly not what was written down.
///
/// The same mistake as a hardcoded platform string, in a different costume: a fact about one device
/// spelled into a file that outlives it. So the invariants that hold for ANY wheel are asserted
/// unconditionally, and the shape is asserted against the class the persona actually selects. The
/// generator's own two shapes are pinned in `korvus-behavior`; what this adds is that the engine
/// carries the persona's platform into the scroll source, and that the shape survives the trip
/// through real event dispatch into page JS.
#[tokio::test(start_paused = true)]
async fn p4_scroll_streams_trusted_wheel_events() {
    use spec::Persona;
    let persona = persona::CapturedPersona::default();
    let kind = behavior::WheelKind::from_platform(&persona.navigator().platform);

    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_document("<!doctype html><html><body></body></html>")
        .expect("load doc");
    e.eval(
        "p4-listen",
        r#"
        globalThis.__wn = 0; globalThis.__wt = 0; globalThis.__badts = 0; globalThis.__frac = 0;
        globalThis.__lastts = -1; globalThis.__badmode = 0; globalThis.__offnotch = 0;
        globalThis.__sum = 0;
        document.addEventListener('wheel', (ev) => {
            globalThis.__wn++;
            if (ev.isTrusted) globalThis.__wt++;
            if (ev.deltaMode !== 0) globalThis.__badmode++;
            if (Math.abs(ev.deltaY % 1) > 1e-6) globalThis.__frac++;
            if (Math.abs(ev.deltaY % 120) > 1e-6) globalThis.__offnotch++;
            if (ev.timeStamp < globalThis.__lastts) globalThis.__badts++;
            globalThis.__lastts = ev.timeStamp;
            globalThis.__sum += ev.deltaY;
        });
        "#
        .to_string(),
    )
    .expect("eval listen");
    e.run().await.expect("run listen");

    let n = e.scroll(600.0).await.expect("scroll");
    e.run().await.expect("run scroll");

    e.eval(
        "p4-report",
        "print('P4:' + [__wn, __wt, __badts, __frac, __badmode, __offnotch, Math.round(__sum)].join(','));"
            .to_string(),
    )
    .expect("eval report");
    e.run().await.expect("run report");
    let out = e.take_output();
    let line = out
        .iter()
        .find(|l| l.starts_with("P4:"))
        .unwrap_or_else(|| panic!("no P4 report; output: {out:?}"))
        .clone();
    let nums: Vec<i64> = line[3..].split(',').map(|s| s.parse().unwrap()).collect();
    let (wn, wt, badts, frac, badmode, offnotch, sum) =
        (nums[0] as usize, nums[1] as usize, nums[2], nums[3], nums[4], nums[5], nums[6]);

    // True of every wheel device.
    assert_eq!(wn, n, "listener saw {wn} wheel events, engine emitted {n}");
    assert_eq!(wt, n, "every wheel event must be isTrusted ({wt}/{n})");
    assert_eq!(badmode, 0, "deltaMode must be 0 (pixel) for a modern profile");
    assert_eq!(badts, 0, "wheel event timeStamps must be monotonic");
    assert!(n >= 2, "a gesture is a stream, not one jump: got {n} event(s)");
    assert!((sum - 600).abs() <= 120, "the gesture must travel roughly 600px, went {sum}");

    // And the shape of the device this persona is holding.
    match kind {
        behavior::WheelKind::Trackpad => {
            assert!(n >= 12, "a trackpad scroll emits many events, got {n}");
            assert!(frac > 0, "trackpad deltas must be fractional (not clean 120-notches)");
        }
        behavior::WheelKind::Mouse => {
            assert!((3..=8).contains(&n), "a 600px mouse scroll is a few notches, got {n}");
            assert_eq!(offnotch, 0, "mouse deltaY must be a multiple of the 120px notch");
            assert_eq!(frac, 0, "a mouse notch is a whole number of pixels");
        }
    }
}

/// P5 — end-to-end interaction coupling: click_selector moves the cursor to the element (many trusted
/// mousemoves) and THEN dispatches a trusted click on it at the arrival point — never a teleport-to-click.
#[tokio::test(start_paused = true)]
async fn p5_click_selector_moves_then_clicks_trusted() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_document("<!doctype html><html><body><button id=go>Go</button></body></html>")
        .expect("load doc");
    e.eval(
        "p5-listen",
        r#"
        globalThis.__moves = 0; globalThis.__clicked = 0; globalThis.__ct = false;
        globalThis.__lmx = null; globalThis.__lmy = null; globalThis.__atMove = false;
        globalThis.__pd = 0; globalThis.__pdt = true; globalThis.__pe = 0;
        document.addEventListener('mousemove', (ev) => {
            globalThis.__moves++; globalThis.__lmx = ev.clientX; globalThis.__lmy = ev.clientY;
        });
        const go = document.getElementById('go');
        go.addEventListener('pointerenter', () => { globalThis.__pe++; });
        go.addEventListener('pointerdown', (ev) => { globalThis.__pd++; if (!ev.isTrusted) globalThis.__pdt = false; });
        go.addEventListener('click', (ev) => {
            globalThis.__clicked++; globalThis.__ct = ev.isTrusted;
            if (ev.clientX === globalThis.__lmx && ev.clientY === globalThis.__lmy) globalThis.__atMove = true;
        });
        "#
        .to_string(),
    )
    .expect("eval listen");
    e.run().await.expect("run listen");

    let _n = e
        .click_selector("#go", 30.0)
        .await
        .expect("click")
        .expect("selector must match");
    e.run().await.expect("run click");

    e.eval(
        "p5-report",
        "print('P5:' + __moves + ',' + __clicked + ',' + __ct + ',' + __atMove + ',' + __pd + ',' + __pdt + ',' + __pe + ',' + (document.activeElement && document.activeElement.id));".to_string(),
    )
    .expect("eval report");
    e.run().await.expect("run report");
    let out = e.take_output();
    let line = out
        .iter()
        .find(|l| l.starts_with("P5:"))
        .unwrap_or_else(|| panic!("no P5 report; output: {out:?}"))
        .clone();
    let parts: Vec<&str> = line[3..].split(',').collect();
    let moves: i64 = parts[0].parse().unwrap();
    let clicked: i64 = parts[1].parse().unwrap();
    let trusted = parts[2] == "true";
    let at_move = parts[3] == "true";
    let pd: i64 = parts[4].parse().unwrap();
    let pdt = parts[5] == "true";
    let pe: i64 = parts[6].parse().unwrap();
    let active_id = parts[7];

    // FIXER C6: one mousemove per display frame (Chrome's rAF-aligned input), not one per 8 ms sample.
    assert!(moves >= 15, "cursor must travel to the target (many mousemoves), got {moves}");
    assert_eq!(clicked, 1, "the element must receive exactly one click");
    assert!(trusted, "the click on the element must be isTrusted");
    assert!(at_move, "the click coord must match the arrival point (arrived then clicked, no teleport)");
    assert_eq!(pd, 1, "the element must receive a pointerdown (full pointer graph, not just mouse events)");
    assert!(pdt, "the pointerdown must be isTrusted");
    assert_eq!(pe, 1, "the element must receive a pointerenter (hover onset before the click)");
    assert_eq!(active_id, "go", "the clicked element must become document.activeElement (focus-on-click)");
}

/// The automation's selector lookup is the realm's ORIGINAL `querySelector`: a page that wrapped
/// `Document.prototype.querySelector` (or Element's) sees no call when a client clicks by selector. It used to
/// look the method up on `document` by name, so the client's selector arrived in the page's wrapper with no JS
/// caller on the stack -- a tell no Chrome gives, since CDP-driven input never runs page methods.
#[tokio::test(start_paused = true)]
async fn click_selector_runs_no_page_replaced_query_method() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_document("<!doctype html><html><body><button id=go>Go</button></body></html>").expect("load doc");
    e.eval(
        "wrap",
        r#"
        globalThis.__seen = [];
        for (const P of [Document.prototype, Element.prototype]) {
            const q = P.querySelector, qa = P.querySelectorAll;
            P.querySelector = function (s) { __seen.push("q:" + s); return q.call(this, s); };
            P.querySelectorAll = function (s) { __seen.push("qa:" + s); return qa.call(this, s); };
        }
        globalThis.__clicked = 0;
        document.getElementById("go").addEventListener("click", () => { __clicked++; });
        "#
        .to_string(),
    )
    .expect("eval wrap");
    e.run().await.expect("run wrap");
    e.click_selector("#go", 30.0).await.expect("click").expect("selector must match");
    e.run().await.expect("run click");
    e.eval("report", "print('seen=' + JSON.stringify(__seen) + '|clicked=' + __clicked);".to_string()).expect("eval report");
    e.run().await.expect("run report");
    assert_eq!(e.take_output(), vec!["seen=[]|clicked=1"]);
}

/// P6 — ambient activity: an idle/reading dwell emits substantial trusted mouse + scroll activity (a
/// session is never zero-behaviour), all isTrusted with monotonic timeStamps across channels.
#[tokio::test(start_paused = true)]
async fn p6_ambient_emits_nonzero_trusted_activity() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_document("<!doctype html><html><body></body></html>")
        .expect("load doc");
    e.eval(
        "p6-listen",
        r#"
        globalThis.__mm = 0; globalThis.__pm = 0; globalThis.__wh = 0; globalThis.__untrusted = 0;
        globalThis.__hv = 0;
        globalThis.__lastts = -1; globalThis.__badts = 0;
        const rec = (ev) => {
            if (ev.type === 'mousemove') globalThis.__mm++;
            else if (ev.type === 'pointermove') globalThis.__pm++;
            else if (ev.type === 'wheel') globalThis.__wh++;
            else globalThis.__hv++;   // the hover boundary events an idle drift crosses
            if (!ev.isTrusted) globalThis.__untrusted++;
            if (ev.timeStamp < globalThis.__lastts) globalThis.__badts++;
            globalThis.__lastts = ev.timeStamp;
        };
        document.addEventListener('mousemove', rec);
        document.addEventListener('pointermove', rec);
        document.addEventListener('wheel', rec);
        // Capture, because mouseenter/mouseleave do not bubble to document. FIXER C6: and the pointer
        // boundary family, which fires with the mouse one, as in Chrome.
        for (const type of ['mouseover', 'mouseout', 'mouseenter', 'mouseleave',
                            'pointerover', 'pointerout', 'pointerenter', 'pointerleave']) {
            document.addEventListener(type, rec, true);
        }
        "#
        .to_string(),
    )
    .expect("eval listen");
    e.run().await.expect("run listen");

    let n = e.ambient(6).await.expect("ambient");
    e.run().await.expect("run ambient");

    e.eval(
        "p6-report",
        "print('P6:' + __mm + ',' + __pm + ',' + __wh + ',' + __untrusted + ',' + __badts + ',' + __hv);".to_string(),
    )
    .expect("eval report");
    e.run().await.expect("run report");
    let out = e.take_output();
    let line = out
        .iter()
        .find(|l| l.starts_with("P6:"))
        .unwrap_or_else(|| panic!("no P6 report; output: {out:?}"))
        .clone();
    let nums: Vec<i64> = line[3..].split(',').map(|s| s.parse().unwrap()).collect();
    let (mm, pm, wh, untrusted, badts) = (nums[0], nums[1], nums[2], nums[3], nums[4]);
    let hv = nums[5];

    assert!(n >= 20, "ambient must emit substantial activity, got {n}");
    assert!(mm > 0, "ambient must produce idle cursor movement (never zero-behaviour)");
    assert!(wh > 0, "ambient must produce a reading scroll");
    assert_eq!(untrusted, 0, "every ambient event must be isTrusted");
    assert_eq!(badts, 0, "ambient timeStamps must be monotonic across channels");
    assert_eq!(
        mm + pm + wh + hv,
        n as i64,
        "listener saw {} events, engine emitted {n}",
        mm + pm + wh + hv
    );
}

/// Keystroke into a focused field: type_into focuses the input (sets document.activeElement), dispatches
/// trusted keydown/keyup ON it, and each char updates .value + fires a trusted `input` event.
#[tokio::test(start_paused = true)]
async fn keystroke_type_into_focuses_and_fills_the_field() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_document("<!doctype html><html><body><input id=name></body></html>")
        .expect("load doc");
    e.eval(
        "ti-listen",
        r#"
        globalThis.__kd = 0; globalThis.__inp = 0; globalThis.__untrusted = 0;
        const el = document.getElementById('name');
        el.addEventListener('keydown', (ev) => { globalThis.__kd++; if (!ev.isTrusted) globalThis.__untrusted++; });
        el.addEventListener('input', (ev) => { globalThis.__inp++; if (!ev.isTrusted) globalThis.__untrusted++; });
        "#
        .to_string(),
    )
    .expect("eval listen");
    e.run().await.expect("run listen");

    let n = e
        .type_into("#name", "hello")
        .await
        .expect("type")
        .expect("field must match");
    e.run().await.expect("run type");

    e.eval(
        "ti-report",
        "print('TI:' + __kd + ',' + __inp + ',' + __untrusted + ',' + (document.activeElement && document.activeElement.id) + ',' + document.getElementById('name').value);".to_string(),
    )
    .expect("eval report");
    e.run().await.expect("run report");
    let out = e.take_output();
    let line = out
        .iter()
        .find(|l| l.starts_with("TI:"))
        .unwrap_or_else(|| panic!("no TI report; output: {out:?}"))
        .clone();
    let parts: Vec<&str> = line[3..].splitn(5, ',').collect();
    let kd: i64 = parts[0].parse().unwrap();
    let inp: i64 = parts[1].parse().unwrap();
    let untrusted: i64 = parts[2].parse().unwrap();
    let active_id = parts[3];
    let value = parts[4];

    assert!(kd >= 5, "a keydown per char must fire on the focused field, got {kd}");
    assert!(inp >= 5, "an input event per char must fire, got {inp}");
    assert_eq!(untrusted, 0, "all key + input events must be isTrusted");
    assert_eq!(active_id, "name", "the field must be focused (document.activeElement.id), got {active_id}");
    assert_eq!(value, "hello", "the field value must reflect the typed text (typos self-corrected)");
    assert!(n >= 10, "keydown+keyup per char (plus input edits), got {n}");
}

/// Touch: tap + swipe stream genuinely-trusted TouchEvents with a force and an elliptical contact
/// (radiusX != radiusY) — the mobile-persona input channel.
#[tokio::test(start_paused = true)]
async fn touch_tap_and_swipe_stream_trusted_events() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_document("<!doctype html><html><body></body></html>")
        .expect("load doc");
    e.eval(
        "t-listen",
        r#"
        globalThis.__ts = 0; globalThis.__te = 0; globalThis.__tm = 0; globalThis.__untrusted = 0;
        globalThis.__ellip = true; globalThis.__force = false;
        const rec = (ev) => {
            if (ev.type === 'touchstart') globalThis.__ts++;
            else if (ev.type === 'touchend') globalThis.__te++;
            else if (ev.type === 'touchmove') globalThis.__tm++;
            if (!ev.isTrusted) globalThis.__untrusted++;
            const t = ev.changedTouches[0];
            if (t) {
                if (Math.abs(t.radiusX - t.radiusY) < 0.1) globalThis.__ellip = false;
                if (t.force > 0) globalThis.__force = true;
            }
        };
        document.addEventListener('touchstart', rec);
        document.addEventListener('touchmove', rec);
        document.addEventListener('touchend', rec);
        "#
        .to_string(),
    )
    .expect("eval listen");
    e.run().await.expect("run listen");

    let tn = e.tap(100.0, 200.0).await.expect("tap");
    let sn = e.swipe(50.0, 600.0, 300.0, 200.0).await.expect("swipe");
    e.run().await.expect("run touch");

    e.eval(
        "t-report",
        "print('TCH:' + __ts + ',' + __te + ',' + __tm + ',' + __untrusted + ',' + __ellip + ',' + __force);".to_string(),
    )
    .expect("eval report");
    e.run().await.expect("run report");
    let out = e.take_output();
    let line = out
        .iter()
        .find(|l| l.starts_with("TCH:"))
        .unwrap_or_else(|| panic!("no TCH report; output: {out:?}"))
        .clone();
    let p: Vec<&str> = line[4..].split(',').collect();
    let ts: i64 = p[0].parse().unwrap();
    let te: i64 = p[1].parse().unwrap();
    let tm: i64 = p[2].parse().unwrap();
    let untrusted: i64 = p[3].parse().unwrap();
    let ellip = p[4] == "true";
    let force = p[5] == "true";

    assert!(ts >= 2, "a touchstart per tap + swipe, got {ts}");
    assert!(te >= 2, "a touchend per tap + swipe, got {te}");
    assert!(tm >= 3, "swipe touchmoves, got {tm}");
    assert_eq!(untrusted, 0, "every touch event must be isTrusted");
    assert!(ellip, "radiusX must differ from radiusY (a real elliptical finger contact)");
    assert!(force, "touch events must carry a non-zero force somewhere");
    assert_eq!((ts + te + tm) as usize, tn + sn, "listener saw all emitted touch events");
}

/// Live-ish validation: an in-page behavioral detector EMULATOR (a device-check script es_* + Castle rules, run as
/// page JS reading the real event stream) scores the neuromotor behavior as HUMAN, and — the required
/// negative control — FLAGS a naive JS-dispatched teleport bot. "A green oracle without a failing control
/// is invalid". This validates the actual dispatched events (isTrusted, movementX/Y,
/// timeStamp) through the same event-listener path a real detector uses, not the generator's raw samples.
#[tokio::test(start_paused = true)]
async fn behavioral_detector_emulator_passes_human_flags_bot() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_document("<!doctype html><html><body></body></html>")
        .expect("load doc");
    e.eval("det-emulator", include_str!("fixtures/detector-emulator.js").to_string())
        .expect("load emulator");
    e.run().await.expect("run emulator");

    // HUMAN: a real neuromotor move + ambient dwell.
    e.move_mouse(700.0, 500.0, 40.0).await.expect("move");
    e.ambient(4).await.expect("ambient");
    e.run().await.expect("run human");
    e.eval(
        "h-report",
        "{ const s = __detScore(); print('HUM:' + s.human + ',' + s.n + ',' + s.allTrusted + ',' + s.accelVaries + ',' + s.hasPointer + ',' + (s.esSigmdn > 0.01) + ',' + s.moveConsistent); }".to_string(),
    )
    .expect("eval h");
    e.run().await.expect("run h");

    // NEGATIVE CONTROL: a naive teleport bot — JS-dispatched (isTrusted=false), only 4 events, linear.
    e.eval(
        "bot",
        r#"
        __detReset();
        for (let i = 0; i < 4; i++) {
            const ev = new MouseEvent('mousemove', { screenX: i*80, screenY: i*80, clientX: i*80, clientY: i*80, bubbles: true });
            document.dispatchEvent(ev);
        }
        { const s = __detScore(); print('BOT:' + s.human + ',' + s.n + ',' + s.allTrusted + ',' + s.enoughEvents); }
        "#
        .to_string(),
    )
    .expect("eval bot");
    e.run().await.expect("run bot");

    let out = e.take_output();
    let hum = out.iter().find(|l| l.starts_with("HUM:")).unwrap_or_else(|| panic!("no HUM; {out:?}")).clone();
    let bot = out.iter().find(|l| l.starts_with("BOT:")).unwrap_or_else(|| panic!("no BOT; {out:?}")).clone();
    let h: Vec<&str> = hum[4..].split(',').collect();
    let b: Vec<&str> = bot[4..].split(',').collect();

    assert_eq!(h[0], "true", "the neuromotor behavior must score HUMAN; verdict = {hum}");
    assert_eq!(b[0], "false", "the naive teleport bot MUST be flagged (negative control); verdict = {bot}");
    assert_eq!(b[2], "false", "the bot's JS-dispatched events must be untrusted (isTrusted=false)");
    assert_eq!(b[3], "false", "the bot's ~4 events must fail the 'hundreds of events' check");
}

/// W9.6 — a click on an element lands IN the persona's viewport, and (FIXER C6) on the element itself.
///
/// `click_selector` used to assign every element a stable SYNTHETIC viewport coordinate, drawn from the
/// selector string, "because Korvus has no layout"; this gate kept those coordinates inside the page's own
/// `innerWidth/innerHeight` (they had once come from a hardcoded 1280 x 800 box). Korvus lays pages out now,
/// and an element with a box on screen is clicked where it IS -- its centre, through the same pipeline as a
/// real click -- so the gate is stronger: every click is inside the viewport the page reports AND inside the
/// clicked button's own `getBoundingClientRect()`. The synthetic coordinate survives only for an element with
/// no box on screen.
#[tokio::test(start_paused = true)]
async fn w9_6_synthetic_click_coords_lie_in_the_personas_viewport() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    let buttons: String = (0..16).map(|i| format!("<button id=b{i}>{i}</button>")).collect();
    e.load_document(&format!("<!doctype html><html><body>{buttons}</body></html>")).expect("load doc");
    e.eval(
        "w96-listen",
        r#"
        globalThis.__xs = []; globalThis.__ys = []; globalThis.__off = 0;
        document.addEventListener('click', (ev) => {
            __xs.push(ev.clientX); __ys.push(ev.clientY);
            const r = ev.target.getBoundingClientRect();
            if (!(ev.clientX >= r.left && ev.clientX < r.right && ev.clientY >= r.top && ev.clientY < r.bottom)) __off++;
        });
        "#
        .to_string(),
    )
    .expect("eval listen");
    e.run().await.expect("run listen");

    for i in 0..16 {
        e.click_selector(&format!("#b{i}"), 30.0).await.expect("click").expect("selector must match");
    }
    e.run().await.expect("run clicks");

    e.eval(
        "w96-report",
        "print('W96:' + innerWidth + ',' + innerHeight + ',' + Math.max(...__xs) + ',' + Math.max(...__ys) + ',' + __xs.length + ',' + __off);"
            .to_string(),
    )
    .expect("eval report");
    e.run().await.expect("run report");
    let out = e.take_output();
    let line = out.iter().find(|l| l.starts_with("W96:")).unwrap_or_else(|| panic!("no W96 report; output: {out:?}")).clone();
    let p: Vec<i64> = line[4..].split(',').map(|s| s.parse().expect("integers")).collect();
    let (iw, ih, max_x, max_y, n, off) = (p[0], p[1], p[2], p[3], p[4], p[5]);

    assert_eq!(n, 16, "every button must have been clicked once");
    assert!(max_x < iw, "a clientX ({max_x}) landed outside the page's own innerWidth ({iw})");
    assert!(max_y < ih, "a clientY ({max_y}) landed outside the page's own innerHeight ({ih})");
    assert_eq!(off, 0, "every click lands inside the button it was dispatched at");
}
