//! The event family's live gate, in BOTH realms.
//!
//! The nineteen interfaces the goal document names (`Event`, `CustomEvent`, `UIEvent`, `MouseEvent`,
//! `PointerEvent`, `WheelEvent`, `Touch`, `TouchEvent`, `KeyboardEvent`, `InputEvent`, `FocusEvent`,
//! `BeforeUnloadEvent`, `ErrorEvent`, `ProgressEvent`, `MessageEvent`, `CookieChangeEvent`,
//! `MessageChannel`, `Notification` and `EventTarget`) were JS classes inside the kernel
//! (`host.js`, `worker-host.js`, `events.js`). They are `interface!` tables with NATIVE constructors
//! now, and this file is the proof that the cutover changed the surface only where the CAPTURE says
//! it was wrong.
//!
//! The probe (`fixtures/event-family-probe.js`) runs UNMODIFIED in the window, in a worker, and in
//! headless Chrome, so all three dumps are directly diffable. `KORVUS_EVENT_FAMILY_OUT=<dir>` writes
//! `window.json` / `worker.json` there; without it the two tests simply assert.
//!
//! The four table-cutover failure modes each have an assertion here:
//!   1. an instance on the class template's prototype  -> `protoOwnIsTrusted` / `protoIsInstance`
//!   2. a row accepting a foreign receiver             -> `fm.foreign` (every row, both realms)
//!   3. a body reachable where it was a no-op          -> `vals` (every row read off a real instance)
//!   4. an install-order change discarding a decorator -> the member ORDER + the two realms' lists
use engine::Engine;

const PROBE: &str = include_str!("fixtures/event-family-probe.js");

fn out_dir() -> Option<String> {
    std::env::var("KORVUS_EVENT_FAMILY_OUT").ok()
}

fn save(name: &str, json: &str) {
    if let Some(d) = out_dir() {
        let _ = std::fs::create_dir_all(&d);
        let pretty = serde_json::from_str::<serde_json::Value>(json)
            .map(|v| serde_json::to_string_pretty(&v).unwrap_or_else(|_| json.to_string()))
            .unwrap_or_else(|_| json.to_string());
        std::fs::write(format!("{d}/{name}.json"), pretty).expect("write the event-family dump");
    }
}

/// The probe's JSON, evaluated in the WINDOW realm of a loaded document.
async fn dump_window() -> serde_json::Value {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body><div id=\"d\"></div></body></html>").unwrap();
    e.run().await.unwrap();
    let v = e.eval_value("event_family_probe", PROBE.to_string()).expect("the window probe runs");
    let s = v.as_str().expect("the probe returns a JSON string").to_string();
    save("window", &s);
    serde_json::from_str(&s).expect("the window probe's JSON parses")
}

/// The probe's JSON, evaluated inside a REAL worker isolate and posted back (the shape
/// `tests/worker_surface.rs` uses; `Exposed` is what decides which rows a worker sees, so the worker
/// realm has to be measured in a worker).
async fn dump_worker() -> serde_json::Value {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    e.run().await.unwrap();
    let worker_src = format!("self.postMessage({PROBE});");
    let reg = format!(
        "globalThis.__workerScripts = {{ './probe.js': {} }};",
        serde_json::to_string(&worker_src).unwrap()
    );
    e.eval("worker-scripts", reg).unwrap();
    e.run().await.unwrap();
    e.eval(
        "probe-driver",
        "const w = new Worker('./probe.js'); w.onmessage = (ev) => print('WPROBE:' + ev.data);".into(),
    )
    .unwrap();
    let start = std::time::Instant::now();
    let mut out: Vec<String> = Vec::new();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if let Some(line) = out.iter().find(|l| l.starts_with("WPROBE:")) {
            let s = line["WPROBE:".len()..].to_string();
            save("worker", &s);
            return serde_json::from_str(&s).expect("the worker probe's JSON parses");
        }
        if start.elapsed() > std::time::Duration::from_secs(20) {
            panic!("the worker did not post its probe within 20s; output: {out:?}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

/// The nineteen names, and the eight of them the captured WORKER baseline exposes.
const ALL: &[&str] = &[
    "Event", "CustomEvent", "UIEvent", "MouseEvent", "PointerEvent", "WheelEvent", "Touch", "TouchEvent",
    "KeyboardEvent", "InputEvent", "FocusEvent", "BeforeUnloadEvent", "ErrorEvent", "ProgressEvent",
    "MessageEvent", "CookieChangeEvent", "MessageChannel", "Notification", "EventTarget",
];
const IN_WORKER: &[&str] =
    &["Event", "CustomEvent", "ErrorEvent", "ProgressEvent", "MessageEvent", "MessageChannel", "Notification", "EventTarget"];

/// The capture's `ctorLength` for each: what `X.length` must read. A JS `class X(type, init)` reports
/// 2 for every one of these, which is the defect the native constructors close.
const CTOR_LEN: &[(&str, u64)] = &[
    ("Event", 1), ("CustomEvent", 1), ("UIEvent", 1), ("MouseEvent", 1), ("PointerEvent", 1),
    ("WheelEvent", 1), ("Touch", 1), ("TouchEvent", 1), ("KeyboardEvent", 1), ("InputEvent", 1),
    ("FocusEvent", 1), ("BeforeUnloadEvent", 0), ("ErrorEvent", 1), ("ProgressEvent", 1),
    ("MessageEvent", 1), ("CookieChangeEvent", 1), ("MessageChannel", 0), ("Notification", 1),
    ("EventTarget", 0),
];

fn iface<'a>(d: &'a serde_json::Value, n: &str) -> &'a serde_json::Value {
    let v = &d[n];
    assert!(!v.is_null(), "{n} is missing from the probe");
    assert!(v.get("absent").is_none(), "{n} is not exposed: {v}");
    v
}

#[tokio::test]
async fn the_window_exposes_all_nineteen_with_the_captures_constructor_lengths() {
    let d = dump_window().await;
    for n in ALL {
        let i = iface(&d, n);
        assert_eq!(i["ctor"]["nat"], serde_json::json!(true), "{n}'s interface object must be [native code]");
        assert_eq!(i["ctor"]["name"], serde_json::json!(*n), "{n}.name");
        let want = CTOR_LEN.iter().find(|(k, _)| k == n).unwrap().1;
        assert_eq!(i["ctor"]["length"].as_u64(), Some(want), "{n}.length (the capture says {want})");
    }
}

/// FAILURE MODE 1: the prototype must not be a live instance of itself, and `isTrusted` -- the ONE
/// own property a real event carries -- must be an own, enumerable, NON-configurable native accessor
/// on the instance and on NO prototype.
#[tokio::test]
async fn is_trusted_is_unforgeable_on_the_instance_and_on_no_prototype() {
    for d in [dump_window().await, dump_worker().await] {
        for n in ALL {
            let Some(i) = d.get(n).filter(|v| v.get("absent").is_none()) else { continue };
            assert_eq!(i["fm"]["protoOwnIsTrusted"], serde_json::json!(false), "{n}.prototype owns isTrusted");
            assert_eq!(i["fm"]["protoIsInstance"], serde_json::json!(false), "{n}.prototype is an instance of {n}");
            // Only the Event subclasses carry isTrusted; EventTarget/MessageChannel/Touch/Notification do not.
            let it = &i["inst"]["isTrusted"];
            if it.is_null() {
                continue;
            }
            assert_eq!(it["acc"], serde_json::json!(true), "{n}: isTrusted must be an accessor");
            assert_eq!(it["e"], serde_json::json!(true), "{n}: isTrusted must be enumerable");
            assert_eq!(it["c"], serde_json::json!(false), "{n}: isTrusted must be NON-configurable ([LegacyUnforgeable])");
            assert_eq!(it["nat"], serde_json::json!(true), "{n}: isTrusted's getter must be [native code]");
            assert_eq!(it["gn"], serde_json::json!("get isTrusted"), "{n}: isTrusted's getter name");
            assert_eq!(it["v"], serde_json::json!(false), "{n}: a script-constructed event is not trusted");
        }
    }
}

/// FAILURE MODE 2: a prototype row must reject a foreign receiver the way Chrome does. Measured
/// against `google-chrome --headless=new` with this very probe: every slot-backed accessor and every
/// method of the family throws `TypeError: Illegal invocation` on `{}`.
#[tokio::test]
async fn no_row_accepts_a_foreign_receiver() {
    for (realm, d) in [("window", dump_window().await), ("worker", dump_worker().await)] {
        for n in ALL {
            let Some(i) = d.get(n).filter(|v| v.get("absent").is_none()) else { continue };
            let Some(f) = i["fm"]["foreign"].as_object() else { continue };
            for (member, verdict) in f {
                let v = verdict.as_str().unwrap_or("");
                assert!(
                    v.starts_with("TypeError"),
                    "{realm}: {n}.prototype.{member} accepted a foreign receiver ({v}) -- Chrome throws Illegal invocation"
                );
            }
        }
    }
}

/// FAILURE MODE 3: every row answers off a real instance -- the init dictionary is parsed in Rust and
/// read back through the table's own accessor. A `stub` row answering `undefined` where the
/// constructor was asked for a value is the regression this catches.
#[tokio::test]
async fn constructors_parse_their_init_dictionary_and_every_row_reads_it_back() {
    let d = dump_window().await;
    let v = |n: &str, m: &str| d[n]["vals"][m].as_str().unwrap_or("<missing>").to_string();
    // Event: the nine base members, from the dictionary and from the defaults.
    assert_eq!(v("Event", "type"), "\"t\"");
    assert_eq!(v("Event", "bubbles"), "false");
    assert_eq!(v("Event", "eventPhase"), "0");
    assert_eq!(v("Event", "target"), "null");
    // timeStamp is the performance clock, read at construction: positive, and finite.
    let ts: f64 = d["Event"]["vals"]["timeStamp"].as_str().unwrap().parse().unwrap();
    assert!(ts > 0.0 && ts.is_finite(), "Event.timeStamp must be a positive DOMHighResTimeStamp, got {ts}");
    assert_eq!(v("CustomEvent", "detail"), "7");
    assert_eq!(v("UIEvent", "detail"), "0");
    assert_eq!(v("MouseEvent", "clientX"), "5");
    assert_eq!(v("MouseEvent", "x"), "5", "x is the clientX alias");
    assert_eq!(v("MouseEvent", "y"), "6", "y is the clientY alias");
    assert_eq!(v("MouseEvent", "which"), "3", "UIEvent.which on a mouse event is button + 1");
    assert_eq!(v("MouseEvent", "fromElement"), "null");
    assert_eq!(v("MouseEvent", "offsetX"), "5", "an untargeted event's offsetX is its clientX");
    assert_eq!(v("PointerEvent", "pointerId"), "3");
    assert_eq!(v("PointerEvent", "pointerType"), "\"\"", "Chrome's default pointerType is the empty string, not \"mouse\"");
    assert_eq!(v("PointerEvent", "isPrimary"), "false", "a constructed PointerEvent describes no real device");
    assert_eq!(v("WheelEvent", "deltaY"), "9");
    assert_eq!(v("WheelEvent", "wheelDeltaY"), "9", "Brave 151 passes deltaY through to wheelDeltaY (measured)");
    assert_eq!(v("KeyboardEvent", "key"), "\"a\"");
    assert_eq!(v("KeyboardEvent", "which"), "65", "which is the keyCode on a key event");
    assert_eq!(v("InputEvent", "data"), "\"x\"");
    assert_eq!(v("FocusEvent", "relatedTarget"), "null");
    assert_eq!(v("ErrorEvent", "message"), "\"m\"");
    assert_eq!(v("ErrorEvent", "lineno"), "3");
    assert_eq!(v("ErrorEvent", "error"), "undefined", "an ABSENT `any error` is undefined; only an explicit null is null");
    // `loaded`/`total` are DOUBLE, not unsigned long long: 1.5 and -5.5 round-trip (WPT
    // progressevent-constructor), and `lengthComputable` is a plain boolean coercion.
    assert_eq!(v("ProgressEvent", "loaded"), "1.5");
    assert_eq!(v("ProgressEvent", "total"), "-5.5");
    assert_eq!(v("ProgressEvent", "lengthComputable"), "true");
    assert_eq!(v("MessageEvent", "origin"), "\"o\"");
    assert_eq!(v("MessageEvent", "lastEventId"), "\"\"");
    assert_eq!(v("MessageEvent", "source"), "null");
    assert_eq!(v("CookieChangeEvent", "changed"), "[object Array]");
    assert_eq!(v("MessageChannel", "port1"), "[object MessagePort]");
    assert_eq!(v("Notification", "body"), "\"b\"");
    assert_eq!(v("Touch", "identifier"), "1");

    // THE LEGACY POSITIONAL INITIALISERS, which is where a writer and a reader can drift onto two
    // different private slots without anything noticing. `initCustomEvent` did exactly that during
    // this unit: `detail` became a table row over the shared `korvus_evt_*` namespace while the
    // initialiser still wrote a `korvus_detail` slot of its own, so `e.initCustomEvent(..., 42)`
    // stored 42 where nothing read it. Every one of these round-trips now.
    let i = |k: &str| d["init"][k].as_str().unwrap_or("<missing>").to_string();
    assert_eq!(i("initEvent"), "b,true,true");
    assert_eq!(i("initCustomEvent"), "b,true,42");
    assert_eq!(i("initCustomEventNoDetail"), "null", "initCustomEvent with no 4th argument sets detail to null");
    assert_eq!(i("initUIEvent"), "b,5");
    assert_eq!(i("initMouseEvent"), "b,1,4");
    assert_eq!(i("initKeyboardEvent"), "b,q,2");
    assert_eq!(i("initMessageEvent"), "b,true,1,org,lid");
}

/// The WebIDL argument contract: `type` is required, the interface object is not callable without
/// `new`, a non-object `eventInitDict` is a TypeError, and a Symbol `type` is a TypeError. Every
/// answer below is headless Chrome's, taken with this same probe.
#[tokio::test]
async fn the_constructors_throw_what_chrome_throws() {
    let d = dump_window().await;
    let e = |n: &str, k: &str| d[n]["fm"]["ctorErrs"][k].as_str().unwrap_or("<missing>").to_string();
    for n in ALL {
        // `new X()` with no argument: a TypeError for every one that requires `type`, and fine for
        // the three whose first argument is optional or absent.
        let need_type = !matches!(*n, "BeforeUnloadEvent" | "MessageChannel" | "EventTarget" | "Touch" | "Notification");
        if need_type {
            assert!(e(n, "noargs").starts_with("TypeError"), "new {n}() must be a TypeError, got {}", e(n, "noargs"));
        }
        // `Touch` and `Notification` also require one argument (measured on Chrome), but the argument
        // is a dictionary / a title rather than an event `type`, so their messages name that instead.
        if matches!(*n, "Touch" | "Notification") {
            assert!(e(n, "noargs").starts_with("TypeError"), "new {n}() must be a TypeError, got {}", e(n, "noargs"));
        }
        // `BeforeUnloadEvent` is NOT constructible at all: Chrome answers "Illegal constructor".
        if *n == "BeforeUnloadEvent" {
            assert!(e(n, "noargs").contains("Illegal constructor"), "new BeforeUnloadEvent() must be an Illegal constructor TypeError, got {}", e(n, "noargs"));
        }
        assert!(e(n, "callnonew").starts_with("TypeError"), "{n}('t') without new must be a TypeError, got {}", e(n, "callnonew"));
        if need_type {
            // The MESSAGE matters, not just the type: Chrome prefixes "Failed to construct 'X': ",
            // and V8's own `to_string` failure does not -- a page reading `e.message` tells them
            // apart. Measured with this probe on `google-chrome --headless=new`.
            // (the probe truncates a message at 80 characters, so the longest names compare by prefix)
            let want = format!("TypeError:Failed to construct '{n}': Cannot convert a Symbol value to a string");
            let got = e(n, "symboltype");
            assert!(want.starts_with(&got) && got.len() >= 40, "new {n}(Symbol()) must throw Chrome's message; want {want:?}, got {got:?}");
        }
    }
    // A non-object, non-null `eventInitDict` is a TypeError in Chrome.
    for n in ["Event", "CustomEvent", "UIEvent", "MouseEvent", "ProgressEvent", "MessageEvent"] {
        assert!(e(n, "baddict").starts_with("TypeError"), "new {n}('t', 5) must be a TypeError, got {}", e(n, "baddict"));
    }
}

/// FAILURE MODE 4: one definition, `Exposed` decides the realm. The eight the captured worker
/// baseline lists are in the worker with the SAME prototype member lists and the same chains as the
/// window; the eleven it does not list are absent there.
#[tokio::test]
async fn one_table_two_realms() {
    let win = dump_window().await;
    let wrk = dump_worker().await;
    for n in ALL {
        let in_worker = IN_WORKER.contains(n);
        let present = wrk.get(n).map(|v| v.get("absent").is_none()).unwrap_or(false);
        assert_eq!(present, in_worker, "{n}: worker exposure disagrees with the captured worker baseline");
        if !in_worker {
            continue;
        }
        let (w, k) = (iface(&win, n), iface(&wrk, n));
        assert_eq!(w["protoOrder"], k["protoOrder"], "{n}: the two realms' prototype member lists differ");
        assert_eq!(w["chain"], k["chain"], "{n}: the two realms' prototype chains differ");
        assert_eq!(w["ctor"]["length"], k["ctor"]["length"], "{n}: the two realms' constructor lengths differ");
        assert_eq!(w["tag"], k["tag"], "{n}: the two realms' Symbol.toStringTag descriptors differ");
    }
    // The worker's `Event.timeStamp` is the same performance clock the window reads (it used to be a
    // hard 0 in `worker-host.js`, which WPT's `timeStamp > 0` rules out).
    let ts: f64 = wrk["Event"]["vals"]["timeStamp"].as_str().unwrap().parse().unwrap();
    assert!(ts > 0.0 && ts.is_finite(), "a worker Event's timeStamp must be positive, got {ts}");
}

/// The chains are the interface: `PointerEvent -> MouseEvent -> UIEvent -> Event` is observable
/// through `getPrototypeOf` and through `instanceof` on every listener a page registers.
#[tokio::test]
async fn the_prototype_chains_are_the_captures() {
    let d = dump_window().await;
    let chain = |n: &str| {
        d[n]["chain"].as_array().unwrap().iter().map(|v| v.as_str().unwrap_or("?").to_string()).collect::<Vec<_>>()
    };
    assert_eq!(chain("Event"), ["Object"]);
    assert_eq!(chain("CustomEvent"), ["Event", "Object"]);
    assert_eq!(chain("UIEvent"), ["Event", "Object"]);
    assert_eq!(chain("MouseEvent"), ["UIEvent", "Event", "Object"]);
    assert_eq!(chain("PointerEvent"), ["MouseEvent", "UIEvent", "Event", "Object"]);
    assert_eq!(chain("WheelEvent"), ["MouseEvent", "UIEvent", "Event", "Object"]);
    assert_eq!(chain("KeyboardEvent"), ["UIEvent", "Event", "Object"]);
    assert_eq!(chain("InputEvent"), ["UIEvent", "Event", "Object"]);
    assert_eq!(chain("FocusEvent"), ["UIEvent", "Event", "Object"]);
    assert_eq!(chain("TouchEvent"), ["UIEvent", "Event", "Object"]);
    assert_eq!(chain("BeforeUnloadEvent"), ["Event", "Object"]);
    assert_eq!(chain("ErrorEvent"), ["Event", "Object"]);
    assert_eq!(chain("ProgressEvent"), ["Event", "Object"]);
    assert_eq!(chain("MessageEvent"), ["Event", "Object"]);
    assert_eq!(chain("CookieChangeEvent"), ["Event", "Object"]);
    assert_eq!(chain("Notification"), ["EventTarget", "Object"]);
    assert_eq!(chain("Touch"), ["Object"]);
    assert_eq!(chain("MessageChannel"), ["Object"]);
    assert_eq!(chain("EventTarget"), ["Object"]);
    // A constructed instance owns nothing but `isTrusted` -- a real event's `Object.keys()` is empty
    // and everything it answers comes off its prototype. `BeforeUnloadEvent` has no instance to
    // inspect here: its constructor throws "Illegal constructor" (Chrome), so the probe recorded the
    // error instead, and `document.createEvent` is checked by `event_model_live`.
    for n in ALL {
        let Some(own) = d[n]["inst"]["own"].as_array() else {
            assert_eq!(*n, "BeforeUnloadEvent", "{n} could not be constructed: {}", d[n]["inst"]);
            continue;
        };
        let extra: Vec<&str> = own.iter().filter_map(|v| v.as_str()).filter(|s| *s != "isTrusted").collect();
        assert!(extra.is_empty(), "a {n} instance owns {extra:?} -- a real one owns only isTrusted");
    }
}
