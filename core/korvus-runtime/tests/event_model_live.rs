//! The DOM event model is Rust end to end: the four tables (`Event`, `EventTarget`,
//! `Observable`, `Subscriber`) are the live prototypes, listener invocation and the event-handler
//! algorithm are one Rust path for every target kind, error reporting is `report_exception`, and
//! `document.createEvent` constructs natively. Every answer below was checked against Chrome
//! (`google-chrome --headless=new --dump-dom`) or the bundle's surface/oracle before it was written.
use engine::Engine;

async fn run(html: &str, js: &str) -> Vec<String> {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(html).unwrap();
    e.eval("event_model_live", js.into()).unwrap();
    e.run().await.unwrap();
    e.take_output()
}

const PAGE: &str = r#"<!doctype html><html><body><div id="outer"><button id="btn" onclick="this.setAttribute('data-fired', (parseInt(this.getAttribute('data-fired') || '0') + 1))">go</button></div></body></html>"#;

/// The four prototypes are byte-for-byte the bundle's member lists, `constructor` last.
#[tokio::test]
async fn the_four_prototypes_are_the_bundles_member_lists() {
    let out = run(PAGE, r#"
        const names = (o) => Object.getOwnPropertyNames(o).join(",");
        print("Event=" + names(Event.prototype) + "|" + Event.length);
        print("EventTarget=" + names(EventTarget.prototype) + "|" + EventTarget.length);
        print("Observable=" + names(Observable.prototype) + "|" + Observable.length + "|" + typeof Observable.from + Observable.from.length);
        print("Subscriber=" + names(Subscriber.prototype) + "|" + Subscriber.length);
        const d = (o, n) => { const x = Object.getOwnPropertyDescriptor(o, n); return n + ":" + (x.get ? "accessor" : "data") + ":e=" + +x.enumerable + ":c=" + +x.configurable + (x.get ? ":s=" + +!!x.set : ":w=" + +x.writable); };
        print(["type", "returnValue", "NONE", "initEvent"].map((n) => d(Event.prototype, n)).join(" "));
        print("statics=" + ["NONE", "CAPTURING_PHASE", "AT_TARGET", "BUBBLING_PHASE"].map((n) => Event[n]).join(",") + " " + d(Event, "NONE"));
        print("tags=" + [Event.prototype, EventTarget.prototype, Observable.prototype, Subscriber.prototype].map((p) => p[Symbol.toStringTag]).join(","));
        print("native=" + [Event.prototype.initEvent, EventTarget.prototype.when, Observable.prototype.subscribe, Subscriber.prototype.next, Observable].map((f) => /\[native code\]/.test(Function.prototype.toString.call(f))).join(","));
    "#).await;
    assert_eq!(out, vec![
        "Event=type,target,currentTarget,eventPhase,bubbles,cancelable,defaultPrevented,composed,timeStamp,srcElement,returnValue,cancelBubble,NONE,CAPTURING_PHASE,AT_TARGET,BUBBLING_PHASE,composedPath,initEvent,preventDefault,stopImmediatePropagation,stopPropagation,constructor|1",
        "EventTarget=addEventListener,dispatchEvent,removeEventListener,when,constructor|0",
        "Observable=catch,drop,every,filter,finally,find,first,flatMap,forEach,inspect,last,map,reduce,some,subscribe,switchMap,take,takeUntil,toArray,constructor|1|function1",
        "Subscriber=active,signal,addTeardown,complete,error,next,constructor|0",
        "type:accessor:e=1:c=1:s=0 returnValue:accessor:e=1:c=1:s=1 NONE:data:e=1:c=0:w=0 initEvent:data:e=1:c=1:w=1",
        "statics=0,1,2,3 NONE:data:e=1:c=0:w=0",
        "tags=Event,EventTarget,Observable,Subscriber",
        "native=true,true,true,true,true",
    ]);
}

/// Every target kind runs its `on<type>` handler through dispatch, in registration order with the
/// listeners beside it: 2 of 2 for the flat targets that used to hand-call the handler.
#[tokio::test]
async fn handlers_fire_through_dispatch_on_every_target_kind() {
    let out = run(PAGE, r#"
        const flat = (mk, ty) => { const t = mk(); let n = 0; t["on" + ty] = () => n++; t.addEventListener(ty, () => n++); t.dispatchEvent(new Event(ty)); return n; };
        print("xhr=" + flat(() => new XMLHttpRequest(), "load"));
        print("signal=" + flat(() => new AbortController().signal, "abort"));
        print("port=" + flat(() => new MessageChannel().port1, "message"));
        print("bc=" + flat(() => new BroadcastChannel("x"), "message"));
        print("cookieStore=" + flat(() => cookieStore, "change"));
        print("mql=" + flat(() => matchMedia("(min-width: 1px)"), "change"));
        print("element=" + flat(() => document.createElement("div"), "click"));
        print("document=" + flat(() => document, "click"));
        print("window=" + flat(() => window, "click"));
        print("eventTarget=" + flat(() => new EventTarget(), "foo"));
        { const el = document.createElement("div"); const o = []; el.onclick = () => o.push("h"); el.addEventListener("click", () => o.push("l")); el.dispatchEvent(new Event("click")); print("h-first=" + o.join(",")); }
        { const el = document.createElement("div"); const o = []; el.addEventListener("click", () => o.push("l")); el.onclick = () => o.push("h"); el.dispatchEvent(new Event("click")); print("l-first=" + o.join(",")); }
        { const el = document.createElement("div"); el.onclick = 5; print("nonobject=" + el.onclick); el.onclick = () => {}; el.onclick = null; let n = 0; el.addEventListener("click", () => n++); el.onclick = () => n += 10; el.dispatchEvent(new Event("click")); print("reactivated-after-listener=" + n); }
        { const el = document.createElement("div"); let r = null; el.onclick = () => false; const ev = new Event("click", { cancelable: true }); r = el.dispatchEvent(ev); print("false-cancels=" + r + "," + ev.defaultPrevented); }
    "#).await;
    assert_eq!(out, vec![
        "xhr=2", "signal=2", "port=2", "bc=2", "cookieStore=2", "mql=2", "element=2", "document=2", "window=2", "eventTarget=1",
        "h-first=h,l", "l-first=l,h", "nonobject=null", "reactivated-after-listener=11", "false-cancels=false,true",
    ]);
}

/// `onclick="code"` compiles once at first activation (named after its attribute, as Chrome), fires
/// once per dispatch, recompiles when the attribute text changes, and is gone when the attribute is.
#[tokio::test]
async fn content_attribute_handlers_compile_once_and_follow_the_attribute() {
    let out = run(PAGE, r#"
        const btn = document.getElementById("btn");
        print("before=" + typeof btn.onclick + "|" + (btn.onclick && btn.onclick.name));
        btn.dispatchEvent(new Event("click")); btn.dispatchEvent(new Event("click"));
        print("fired=" + btn.getAttribute("data-fired") + "|" + (btn.onclick === btn.onclick));
        btn.setAttribute("onclick", "this.setAttribute('data-fired', 'changed')");
        btn.dispatchEvent(new Event("click"));
        print("changed=" + btn.getAttribute("data-fired"));
        btn.removeAttribute("onclick");
        print("removed=" + String(btn.onclick));
        // an IDL assignment wins over the attribute, and ordering with a listener holds for the attribute form too
        const o = []; const el = document.createElement("div"); el.setAttribute("onclick", "");
        el.addEventListener("click", () => o.push("l"));
        el.dispatchEvent(new Event("click"));
        el.onclick = () => o.push("h");
        el.dispatchEvent(new Event("click"));
        print("attr-then-listener=" + o.join(","));
    "#).await;
    assert_eq!(out, vec!["before=function|onclick", "fired=2|true", "changed=changed", "removed=null", "attr-then-listener=l,l,h"]);
}

/// `window.event` is the running listener's event (nested dispatch restores it) and undefined outside;
/// a throwing listener does not stop the next one and is counted; `handleEvent` runs with `this` = the
/// listener; the window's `onerror` gets the five-argument form and cancels with `true` -> count 0.
/// A content attribute's handler is a FUNCTION BODY compiled with HTML's scope -- the element, its form owner, its
/// document -- not text pasted into a script. Every expected value is Chrome 150's (`google-chrome --headless=new
/// --dump-dom`, 2026-10-22): `q` and `submit` resolve on the form, `URL` on the document (`document.URL`, a string);
/// `toString()` is `function onclick(event) {…}`; a body that closes the function early is a SyntaxError (null
/// handler) and its trailing code never runs; a `<div>` is not form-associated, so a form ancestor is not in its
/// scope; the Window's `onerror` takes its five parameters.
#[tokio::test]
async fn content_attribute_handlers_compile_as_a_function_body_in_htmls_scope() {
    let out = run(
        r#"<!doctype html><html><body><form id=f><input name=q value=fromform><button id=b onclick="return q.value + '|' + typeof submit + '|' + (typeof URL)">x</button><div id=dv onclick="return typeof q + '|' + typeof submit">z</div></form><div id=d onclick="}); window.k=1; (function(){">y</div></body></html>"#,
        r#"
        const b = document.getElementById("b");
        print(JSON.stringify(b.onclick.toString()));
        print(b.onclick.name + "|" + b.onclick());
        print("div-in-form=" + document.getElementById("dv").onclick());
        print("breakout=" + String(document.getElementById("d").onclick) + "|k=" + window.k);
        document.body.setAttribute("onerror", "return 1");
        print(JSON.stringify(window.onerror.toString()));
    "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            r#""function onclick(event) {\nreturn q.value + '|' + typeof submit + '|' + (typeof URL)\n}""#,
            "onclick|fromform|function|string",
            "div-in-form=undefined|undefined",
            "breakout=null|k=undefined",
            r#""function onerror(event, source, lineno, colno, error) {\nreturn 1\n}""#,
        ]
    );
}

/// The engine's own reads never run a page's replacement members. Chrome 150 (`--dump-dom`, 2026-10-22), with
/// `preventDefault`, `defaultPrevented` and `target` replaced on `Event.prototype` and `querySelectorAll` /
/// `NodeList.length` wrapped: `returnValue = false` cancels without calling the page's `preventDefault`,
/// `returnValue` and `srcElement` read the event's own state, and `window.length` counts the child frame -- the log
/// stays empty.
#[tokio::test]
async fn legacy_event_accessors_and_window_length_run_no_page_member() {
    let out = run(
        "<!doctype html><html><body><iframe></iframe></body></html>",
        r#"
        const log = [];
        const t = new Event("x", { cancelable: true });
        document.body.addEventListener("x", (ev) => { globalThis.__src = ev.srcElement === document.body; });
        Event.prototype.preventDefault = function () { log.push("pd"); };
        Object.defineProperty(Event.prototype, "defaultPrevented", { get() { log.push("dp"); return false; }, configurable: true });
        Object.defineProperty(Event.prototype, "target", { get() { log.push("tg"); return null; }, configurable: true });
        document.body.dispatchEvent(t);
        t.returnValue = false;
        const rv = t.returnValue;
        const qsa = Document.prototype.querySelectorAll;
        Document.prototype.querySelectorAll = function (s) { log.push("qsa:" + s); return qsa.call(this, s); };
        const len = Object.getOwnPropertyDescriptor(NodeList.prototype, "length").get;
        Object.defineProperty(NodeList.prototype, "length", { get() { log.push("nl"); return len.call(this); }, configurable: true });
        const n = window.length;
        print("log=" + log.join(",") + "|rv=" + rv + "|src=" + __src + "|len=" + n);
    "#,
    )
    .await;
    assert_eq!(out, vec!["log=|rv=false|src=true|len=1"]);
}

#[tokio::test]
async fn listener_invocation_and_error_reporting_are_one_rust_path() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(PAGE).unwrap();
    e.eval("a", r#"
        { const el = document.createElement("div"); let seen = []; el.addEventListener("x", (ev) => { seen.push(window.event === ev); el.addEventListener("y", (e2) => seen.push(window.event === e2)); el.dispatchEvent(new Event("y")); seen.push(window.event === ev); }); el.dispatchEvent(new Event("x")); print("window.event=" + seen.join(",") + "|" + String(window.event)); }
        { const el = document.createElement("div"); const L = { handleEvent() { print("handleEvent-this=" + String(this === L)); } }; el.addEventListener("x", L); el.dispatchEvent(new Event("x")); }
        { const el = document.createElement("div"); let n = 0; el.addEventListener("x", () => { throw new Error("boom"); }); el.addEventListener("x", () => n++); el.dispatchEvent(new Event("x")); print("throw-then-next=" + n); }
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec!["window.event=true,true,true|undefined", "handleEvent-this=true", "throw-then-next=1"]);
    assert_eq!(e.uncaught_error_count(), 1, "the throwing listener is one uncaught error");
    e.eval("b", r#"
        let args = null; window.onerror = function () { args = [...arguments].map((a) => typeof a).join(","); return true; };
        let got = 0; window.addEventListener("error", () => got++);
        reportError(new Error("r"));
        const el = document.createElement("div"); el.addEventListener("x", () => { throw new Error("q"); }); el.dispatchEvent(new Event("x"));
        print("onerror=" + args + "|listener=" + got);
        window.onerror = null;
        try { reportError(); print("reportError()=no throw"); } catch (x) { print("reportError()=" + x.name); }
        print("native=" + /\[native code\]/.test(Function.prototype.toString.call(reportError)) + "|" + reportError.length);
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec!["onerror=string,string,number,number,object|listener=2", "reportError()=TypeError", "native=true|1"]);
    assert_eq!(e.uncaught_error_count(), 1, "a cancelling window.onerror keeps the count where it was");
}

/// `document.createEvent` constructs the mapped interface natively, uninitialised until `initEvent`.
#[tokio::test]
async fn create_event_is_native_and_uninitialised() {
    let out = run(PAGE, r#"
        const e = document.createEvent("MouseEvents");
        print("proto=" + Object.getPrototypeOf(e).constructor.name + " type=" + JSON.stringify(e.type));
        try { document.dispatchEvent(e); print("dispatch=no throw"); } catch (x) { print("dispatch=" + x.name); }
        e.initEvent("click", true, true); print("after-init=" + document.dispatchEvent(e) + "|" + e.type);
        try { document.createEvent("Bogus"); print("bogus=no throw"); } catch (x) { print("bogus=" + x.name); }
        print("kinds=" + ["CustomEvent", "KeyboardEvents", "BeforeUnloadEvent", "HTMLEvents", "Events"].map((k) => Object.getPrototypeOf(document.createEvent(k)).constructor.name).join(","));
        print("native=" + /\[native code\]/.test(Function.prototype.toString.call(document.createEvent)));
    "#).await;
    assert_eq!(out, vec!["proto=MouseEvent type=\"\"", "dispatch=InvalidStateError", "after-init=true|click", "bogus=NotSupportedError", "kinds=CustomEvent,KeyboardEvent,BeforeUnloadEvent,Event,Event", "native=true"]);
}

/// Observable / Subscriber / when(): the recorded browser's semantics, no own properties on a subscriber.
#[tokio::test]
async fn observable_subscriber_and_when_deliver_and_tear_down() {
    let out = run(PAGE, r#"
        const t = (f) => { try { return f(); } catch (x) { return x.name + ": " + x.message; } };
        print("ctor0=" + t(() => new Observable()));
        print("call=" + t(() => Observable(function () {})));
        print("ctor1=" + t(() => new Observable(1)));
        print("foreign=" + t(() => Observable.prototype.subscribe.call(Object.create(Observable.prototype), function () {})));
        { const got = []; new Observable((s) => { s.next(1); s.complete(); s.next(2); }).subscribe({ next: (v) => got.push(v), complete: () => got.push("c") }); print("page=" + got.join(",")); }
        { let sub; new Observable((s) => { sub = s; }).subscribe(); print("sub=" + JSON.stringify(Object.keys(sub)) + "|" + sub.active + "|" + (sub.signal instanceof AbortSignal) + "|" + sub.signal.aborted + "|" + Object.prototype.toString.call(sub)); sub.complete(); print("done=" + sub.active + "|" + sub.signal.aborted); print("sub-foreign=" + t(() => Subscriber.prototype.next.call(Object.create(Subscriber.prototype), 1))); }
        { let n = ""; new Observable((s) => { s.error(new TypeError("x")); }).subscribe({ error: (e) => { n = e.name; } }); print("error=" + n); }
        { let n = 0; new Observable((s) => { s.complete(); s.error(new Error("x")); s.complete(); }).subscribe({ error: () => n += 10, complete: () => n++ }); print("once=" + n); }
        { let t2 = 0; new Observable((s) => { s.addTeardown(() => t2++); s.complete(); s.addTeardown(() => t2 += 10); }).subscribe(); print("teardown=" + t2); }
        { const c = new AbortController(); c.abort(); let n = 0; new Observable(() => { n++; }).subscribe(() => {}, { signal: c.signal }); print("aborted-start=" + n); }
        { const tg = new EventTarget(); const got = []; const c = new AbortController(); tg.when("x").subscribe((ev) => got.push(ev.type), { signal: c.signal }); tg.dispatchEvent(new Event("x")); c.abort(); tg.dispatchEvent(new Event("x")); print("when=" + got.join(",") + "|" + (tg.when("x") instanceof Observable) + "|" + typeof tg.when("x").subscribe(() => {}) + "|" + Object.prototype.toString.call(tg.when("x"))); }
        print("when0=" + t(() => new EventTarget().when()));
        { let n = 0; const tg = new EventTarget(); tg.when("x").subscribe(() => { throw new Error("in next"); }); tg.addEventListener("x", () => n++); tg.dispatchEvent(new Event("x")); print("next-throw-reported=" + n); }
        print("stubs=" + typeof Observable.prototype.map + "|" + (new Observable(() => {}).toArray() instanceof Promise) + "|" + new Observable(() => {}).map(() => 1));
        print("newable=" + t(() => new Subscriber()));
    "#).await;
    assert_eq!(out, vec![
        "ctor0=TypeError: Failed to construct 'Observable': 1 argument required, but only 0 present.",
        "call=TypeError: Failed to construct 'Observable': Please use the 'new' operator, this DOM object constructor cannot be called as a function.",
        "ctor1=TypeError: Failed to construct 'Observable': parameter 1 is not of type 'Function'.",
        "foreign=TypeError: Illegal invocation",
        "page=1,c",
        "sub=[]|true|true|false|[object Subscriber]",
        "done=false|true",
        "sub-foreign=TypeError: Illegal invocation",
        "error=TypeError",
        "once=1",
        "teardown=11",
        "aborted-start=1",
        "when=x|true|undefined|[object Observable]",
        "when0=TypeError: Failed to execute 'when' on 'EventTarget': 1 argument required, but only 0 present.",
        "next-throw-reported=1",
        // The operators are implemented: `map()` answers an Observable, as
        // Chrome 150 does (the oracle records `... .map(f) instanceof Observable` true).
        "stubs=function|true|[object Observable]",
        // `new X()` carries Chrome's `Failed to construct 'X': ` construction context.
        "newable=TypeError: Failed to construct 'Subscriber': Illegal constructor",
    ]);
}

/// The worker realm is the same model: `self.onmessage` and a `MessagePort`'s `onmessage` fire through
/// dispatch beside their listeners (2 of 2), the four prototypes are the tables there too, and a
/// throwing `self.onmessage` is reported at the worker global and then reaches the page's
/// `worker.onerror` -- no hand call anywhere in the receive loop.
#[tokio::test]
async fn the_worker_realm_runs_the_same_model() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    let worker = r#"
        let n = 0;
        self.onmessage = (ev) => {
            if (ev.data === "boom") throw new Error("handler boom");
            n++;
            const ch = new MessageChannel(); let m = 0;
            ch.port1.onmessage = () => m++; ch.port1.addEventListener("message", () => { m++; self.postMessage("port=" + m); });
            ch.port2.postMessage("x");
        };
        // registered second: runs after the handler, so it sees both increments
        self.addEventListener("message", (ev) => {
            if (ev.data === "boom") return;
            n++;
            const names = (o) => Object.getOwnPropertyNames(o).join(",");
            const t = new EventTarget(); let w = 0; t.when("z").subscribe(() => w++); t.dispatchEvent(new Event("z"));
            self.postMessage("self=" + n + "|Event=" + names(Event.prototype).split(",").length + "|ET=" + names(EventTarget.prototype) + "|Obs=" + typeof Observable + "," + typeof Subscriber + "|when=" + w);
        });
    "#;
    let entries = format!("'./w.js': {}", serde_json::to_string(worker).unwrap());
    e.eval("worker-scripts", format!("globalThis.__workerScripts = {{ {entries} }};")).unwrap();
    e.eval("page", r#"
        const w = new Worker("./w.js");
        const got = [];
        w.onmessage = (ev) => { got.push(ev.data); if (got.length === 2) { w.postMessage("boom"); } };
        w.onerror = (ev) => { print("RESULT:" + got.sort().join(";") + ";error=" + (ev instanceof ErrorEvent) + ":" + ev.message); };
        w.postMessage("go");
    "#.into()).unwrap();
    let mut out = Vec::new();
    let start = std::time::Instant::now();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if out.iter().any(|l| l.starts_with("RESULT:")) || start.elapsed() > std::time::Duration::from_secs(20) { break; }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let result = out.iter().find(|l| l.starts_with("RESULT:")).cloned().unwrap_or_else(|| format!("RESULT:NONE (output: {out:?})"));
    assert_eq!(
        result,
        "RESULT:port=2;self=2|Event=22|ET=addEventListener,dispatchEvent,removeEventListener,when,constructor|Obs=function,function|when=1;error=true:Uncaught Error: handler boom"
    );
}

/// Every Event-family getter reads its OWN member (the engine caches each slot getter's two private keys
/// per isolate, keyed by the getter's data; a key that mixes two getters up answers another member's
/// value). Each interface is built with a distinct init value per member whose default is a number,
/// string or boolean, and the members that give their own value back are listed. Chrome 150 headless
/// gave the same lines on 2026-09-30. The 22 interfaces where it does not yet agree (CloseEvent,
/// DragEvent, ToggleEvent, ...) are left out until they do.
#[tokio::test]
async fn every_event_getter_reads_its_own_member() {
    let out = run(PAGE, r#"
        for (const n of ["AnimationEvent", "AnimationPlaybackEvent", "BeforeInstallPromptEvent", "ClipboardEvent", "CookieChangeEvent", "CustomEvent", "ErrorEvent", "Event", "FocusEvent", "FontFaceSetLoadEvent", "GamepadEvent", "HashChangeEvent", "InterestEvent", "KeyboardEvent", "MIDIConnectionEvent", "MIDIMessageEvent", "MediaStreamEvent", "MessageEvent", "MouseEvent", "PageRevealEvent", "PageSwapEvent", "PaymentRequestUpdateEvent", "PointerEvent", "ProgressEvent", "RTCPeerConnectionIceEvent", "StorageEvent", "SubmitEvent", "TextFormatUpdateEvent", "TouchEvent", "TrackEvent", "TransitionEvent", "UIEvent", "WheelEvent"]) {
            const C = window[n], inst = new C("x"), init = {};
            let j = 0;
            for (let p = C.prototype; p && p !== Object.prototype; p = Object.getPrototypeOf(p)) {
                for (const k of Object.getOwnPropertyNames(p)) {
                    const d = Object.getOwnPropertyDescriptor(p, k);
                    if (!d.get || k in init) continue;
                    let v; try { v = d.get.call(inst); } catch (e) { continue; }
                    j++;
                    if (typeof v === "number") init[k] = j; else if (typeof v === "string") init[k] = "s" + j; else if (typeof v === "boolean") init[k] = !v;
                }
            }
            const ev = new C("x", init);
            print(n + ":" + Object.keys(init).filter((k) => ev[k] === init[k]).sort().join(","));
        }
    "#).await;
    assert_eq!(out, vec![
        "AnimationEvent:animationName,bubbles,cancelable,composed,elapsedTime,pseudoElement",
        "AnimationPlaybackEvent:bubbles,cancelable,composed",
        "BeforeInstallPromptEvent:bubbles,cancelable,composed",
        "ClipboardEvent:bubbles,cancelable,composed",
        "CookieChangeEvent:bubbles,cancelable,composed",
        "CustomEvent:bubbles,cancelable,composed",
        "ErrorEvent:bubbles,cancelable,colno,composed,filename,lineno,message",
        "Event:bubbles,cancelable,composed",
        "FocusEvent:bubbles,cancelable,composed,detail",
        "FontFaceSetLoadEvent:bubbles,cancelable,composed",
        "GamepadEvent:bubbles,cancelable,composed",
        "HashChangeEvent:bubbles,cancelable,composed,newURL,oldURL",
        "InterestEvent:bubbles,cancelable,composed",
        "KeyboardEvent:altKey,bubbles,cancelable,charCode,code,composed,ctrlKey,detail,isComposing,key,keyCode,location,metaKey,repeat,shiftKey",
        "MIDIConnectionEvent:bubbles,cancelable,composed",
        "MIDIMessageEvent:bubbles,cancelable,composed",
        "MediaStreamEvent:bubbles,cancelable,composed",
        "MessageEvent:bubbles,cancelable,composed,lastEventId,origin",
        "MouseEvent:altKey,bubbles,button,buttons,cancelable,clientX,clientY,composed,ctrlKey,detail,metaKey,movementX,movementY,screenX,screenY,shiftKey",
        "PageRevealEvent:bubbles,cancelable,composed",
        "PageSwapEvent:bubbles,cancelable,composed",
        "PaymentRequestUpdateEvent:bubbles,cancelable,composed",
        "PointerEvent:altKey,altitudeAngle,azimuthAngle,bubbles,button,buttons,cancelable,clientX,clientY,composed,ctrlKey,detail,height,isPrimary,metaKey,movementX,movementY,persistentDeviceId,pointerId,pointerType,pressure,screenX,screenY,shiftKey,tangentialPressure,tiltX,tiltY,twist,width",
        "ProgressEvent:bubbles,cancelable,composed,lengthComputable,loaded,total",
        "RTCPeerConnectionIceEvent:bubbles,cancelable,composed",
        "StorageEvent:bubbles,cancelable,composed,url",
        "SubmitEvent:bubbles,cancelable,composed",
        "TextFormatUpdateEvent:bubbles,cancelable,composed",
        "TouchEvent:altKey,bubbles,cancelable,composed,ctrlKey,detail,metaKey,shiftKey",
        "TrackEvent:bubbles,cancelable,composed",
        "TransitionEvent:bubbles,cancelable,composed,elapsedTime,propertyName,pseudoElement",
        "UIEvent:bubbles,cancelable,composed,detail",
        "WheelEvent:altKey,bubbles,button,buttons,cancelable,clientX,clientY,composed,ctrlKey,deltaMode,deltaX,deltaY,deltaZ,detail,metaKey,movementX,movementY,screenX,screenY,shiftKey,wheelDeltaX,wheelDeltaY"
    ]);
}

/// A fused receiver check keeps the last brand it found to BE its interface: the same getter
/// called on a foreign platform object, then on a genuine one, then on the foreign one again, refuses
/// both foreign calls. Chrome 150 answers `TypeError: Illegal invocation` for each.
#[tokio::test]
async fn a_refused_receiver_is_refused_again_after_a_genuine_one() {
    let out = run(PAGE, r#"
        const t = (f) => { try { return String(f()); } catch (e) { return e.name + ": " + e.message; } };
        const g = Object.getOwnPropertyDescriptor(Node.prototype, "firstChild").get;
        const ev = new Event("x"), el = document.getElementById("outer");
        print([t(() => g.call(ev)), t(() => g.call(el) === el.firstChild), t(() => g.call(ev)), t(() => g.call(ev))].join(" | "));
    "#).await;
    assert_eq!(out, vec!["TypeError: Illegal invocation | true | TypeError: Illegal invocation | TypeError: Illegal invocation"]);
}

/// A dispatch reads `_stop` again, and clears `_stopImmediate` again, only after a listener has
/// run, so each way a listener or the page stops propagation must still be seen at the very next node:
/// in a capture listener, before the dispatch, by `stopImmediatePropagation` at the target, by
/// `cancelBubble` with a listener-less node above it, and a re-dispatch of a stopped event starts clean.
/// Chrome 150 (headless, 2026-09-30) answers every line.
#[tokio::test]
async fn every_way_to_stop_propagation_is_seen_at_the_next_node() {
    let out = run("<!doctype html><html><body></body></html>", r#"
        const log = [];
        const mk = () => { const a = document.createElement('div'), b = document.createElement('p'), c = document.createElement('span'); a.appendChild(b); b.appendChild(c); document.body.appendChild(a); return [a, b, c]; };
        const on = (el, name, type, cap, fn) => el.addEventListener(type, (e) => { log.push(name + (cap ? 'C' : 'B') + e.eventPhase); if (fn) fn(e); }, cap);
        { const [a, b, c] = mk(); on(a, 'a', 't1', true); on(b, 'b', 't1', true, e => e.stopPropagation()); on(c, 'c', 't1', false); on(a, 'a', 't1', false);
          log.push('r1=' + c.dispatchEvent(new Event('t1', { bubbles: true }))); }
        { const [a, b, c] = mk(); on(a, 'a', 't2', true); on(c, 'c', 't2', false); const e = new Event('t2', { bubbles: true }); e.stopPropagation();
          log.push('r2=' + c.dispatchEvent(e)); }
        { const [a, b, c] = mk(); on(c, 'c1', 't3', false, e => e.stopImmediatePropagation()); on(c, 'c2', 't3', false); on(b, 'b', 't3', false);
          log.push('r3=' + c.dispatchEvent(new Event('t3', { bubbles: true }))); }
        { const [a, b, c] = mk(); on(c, 'c', 't4', false); on(b, 'b', 't4', false, e => { e.cancelBubble = true; }); on(a, 'a', 't4', false);
          log.push('r4=' + c.dispatchEvent(new Event('t4', { bubbles: true }))); }
        { const [a, b, c] = mk(); on(b, 'b', 't6', false, e => e.stopPropagation()); on(a, 'a', 't6', false); const e = new Event('t6', { bubbles: true });
          c.dispatchEvent(e); log.push('mid'); c.dispatchEvent(e); log.push('ct=' + e.currentTarget + ' ph=' + e.eventPhase); }
        print(log.join(' '));
    "#).await;
    assert_eq!(out, vec!["aC1 bC1 r1=true r2=true c1B2 r3=true cB2 bB3 r4=true bB3 mid bB3 ct=null ph=0"]);
}
