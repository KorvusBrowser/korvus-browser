//! `ServiceWorker`, `ServiceWorkerRegistration` and `ServiceWorkerContainer` read back off the LIVE
//! surface.
//!
//! The subsystem was 180 lines of `host.js` that REPLACED thirteen of these prototypes' D2 stub
//! members at runtime, with `op_nativize`-masked JS closures, on prototypes the registry install had
//! already built. This file is the gate on the three tables that took their place: the shape, the
//! receiver behaviour, and the `register()` round trip.
//!
//! EVERY EXPECTATION MARKED "measured" comes from `google-chrome --headless=new` 150.0.7871.46 over a
//! loopback HTTP server, 2026-09-12. Where this engine deliberately answers something else —
//! `getRegistration` keying on the exact scope — the assertion says so, because it is carried over
//! verbatim from the JS and is W5.3/W5.4's to close.

use engine::Engine;

const SERVICE_WORKER: &str = include_str!("fixtures/worker/service-worker.js");

/// A page with the three tables installed; no worker spawned.
async fn probe(script: &str) -> Vec<String> {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body>x</body></html>").unwrap();
    e.eval("t", script.to_string()).unwrap();
    e.run().await.unwrap();
    e.take_output()
}

/// A page that registers a real ServiceWorker isolate, pumped until it prints `RESULT:`.
async fn probe_registered(driver: &str) -> Vec<String> {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    e.eval(
        "worker-scripts",
        format!(
            "globalThis.__workerScripts = {{ {}: {} }};",
            serde_json::to_string("./sw.js").unwrap(),
            serde_json::to_string(SERVICE_WORKER).unwrap()
        ),
    )
    .unwrap();
    e.run().await.unwrap();
    e.eval("driver", driver.to_string()).unwrap();
    let mut out = Vec::new();
    let start = std::time::Instant::now();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if out.iter().any(|l| l.starts_with("RESULT:")) {
            break;
        }
        if start.elapsed() > std::time::Duration::from_secs(20) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    out
}

/// The three prototypes' MEMBER order, and `constructor`'s position, exactly as the capture records
/// them.
///
/// THIS TEST NAMED FP12 AND THEN PINNED THE WRONG ANSWER, which is the most useful thing it did. Its
/// note read: the bundle's `members` list does not record where `constructor` sits; real Chrome
/// (measured) puts it in the MIDDLE — `ServiceWorker.prototype` is `[…, postMessage, constructor,
/// onerror]` and `ServiceWorkerRegistration.prototype` is `[…, paymentManager, constructor,
/// backgroundFetch, …]` — every hand table in this tree puts it LAST, the position is tree-wide, and
/// "closing it needs the capture rig to record `constructor`'s index. Written down here so the next
/// capture can close it."
///
/// The capture records it (`ctorIndex`, every bundle's window `surface.json` since `3a5f0b60`) and
/// the installer reads it, so the three expectations below are no longer literals: they
/// are read out of the bundle this engine is wearing. It answers 4/5 and 10/17 where it used to
/// answer 5/5 and 17/17 — the two rows that note said Chrome disagreed with — and 9/9 on the
/// container, which it already matched.
#[tokio::test]
async fn the_three_prototypes_have_the_recorded_member_order() {
    let out = probe(
        r#"
        for (const n of ["ServiceWorker", "ServiceWorkerRegistration", "ServiceWorkerContainer"]) {
          const keys = Object.getOwnPropertyNames(globalThis[n].prototype);
          print(n + "=" + JSON.stringify(keys.filter((k) => k !== "constructor")));
          print(n + ".constructor=" + keys.indexOf("constructor") + "/" + (keys.length - 1));
        }
        // `[Exposed]` and the chain: all three inherit EventTarget in both captures.
        print("chain=" + ["ServiceWorker", "ServiceWorkerRegistration", "ServiceWorkerContainer"]
            .map((n) => Object.getPrototypeOf(globalThis[n].prototype) === EventTarget.prototype).join(","));
        print("newable=" + ["ServiceWorker", "ServiceWorkerRegistration", "ServiceWorkerContainer"]
            .map((n) => { try { new globalThis[n](); return "constructed"; } catch (e) { return e.constructor.name; } }).join(","));
        "#,
    )
    .await;
    // The seat is the worn bundle's, read the way the engine reads it -- never a number typed here.
    let seat = |iface: &str| -> String {
        use spec::Persona;
        let s = persona::CapturedPersona::default()
            .surface(spec::SurfaceRealm::Window)
            .expect("the shipped bundle records a window surface");
        let o = s.members_of(iface).unwrap_or_else(|| panic!("the capture records {iface}"));
        let at = o.ctor_index.unwrap_or_else(|| panic!("the capture records {iface}'s ctorIndex"));
        format!("{iface}.constructor={at}/{}", o.members.len())
    };
    assert_eq!(
        out,
        vec![
            r#"ServiceWorker=["scriptURL","state","onstatechange","postMessage","onerror"]"#.to_string(),
            // `onerror` is a partial's, so it sits BEHIND `constructor`: 4 of 5, which is Chrome's
            // "fifth of six" and what this file measured in 2026-09-12 and could not yet assert.
            seat("ServiceWorker"),
            r#"ServiceWorkerRegistration=["installing","waiting","active","navigationPreload","scope","updateViaCache","onupdatefound","unregister","update","paymentManager","backgroundFetch","periodicSync","sync","cookies","pushManager","getNotifications","showNotification"]"#.to_string(),
            // Seven partial members (`backgroundFetch` on) sit behind it: 10 of 17.
            seat("ServiceWorkerRegistration"),
            r#"ServiceWorkerContainer=["controller","ready","oncontrollerchange","onmessage","onmessageerror","getRegistration","getRegistrations","register","startMessages"]"#.to_string(),
            // No partial extends this one, so the rule puts it last -- 9 of 9, unchanged.
            seat("ServiceWorkerContainer"),
            "chain=true,true,true".to_string(),
            "newable=TypeError,TypeError,TypeError".to_string(),
        ]
    );
}

/// `navigator.serviceWorker`: a real `ServiceWorkerContainer` instance with NO own property, the
/// right brand, and a `ready` that is the same promise on every read — all four measured.
#[tokio::test]
async fn the_container_is_an_instance_with_no_own_properties() {
    let out = probe(
        r#"
        const c = navigator.serviceWorker;
        print("own=" + JSON.stringify(Object.getOwnPropertyNames(c)));
        print("tag=" + Object.prototype.toString.call(c));
        print("instance=" + (c instanceof ServiceWorkerContainer));
        print("same=" + (navigator.serviceWorker === navigator.serviceWorker));
        print("controller=" + String(c.controller));
        print("readySame=" + (c.ready === c.ready) + " " + (c.ready instanceof Promise));
        // Measured: with no registration `ready` is still PENDING after a macrotask --
        // Chrome answers the 300 ms timer in `Promise.race([navigator.serviceWorker.ready, timer])`.
        Promise.race([c.ready.then(() => "resolved"), new Promise((r) => setTimeout(() => r("pending"), 50))])
          .then((v) => print("readyNoRegistration=" + v));
        // The `serviceWorker` row itself is a native getter on Navigator.prototype.
        const d = Object.getOwnPropertyDescriptor(Navigator.prototype, "serviceWorker");
        print("row=" + String(d.get).includes("[native code]") + " " + d.enumerable + " " + d.configurable);
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "own=[]",
            "tag=[object ServiceWorkerContainer]",
            "instance=true",
            "same=true",
            "controller=null",
            "readySame=true true",
            "row=true true true",
            "readyNoRegistration=pending",
        ]
    );
}

/// THE RECEIVER SPLIT. The JS bodies had no receiver check at all and every one answered its shape
/// default; Chrome splits them the way WebIDL does, and this is that split, measured member by
/// member: a plain attribute or operation THROWS, a promise-returning one REJECTS.
#[tokio::test]
async fn no_row_accepts_a_foreign_receiver_and_the_promise_rows_reject() {
    let out = probe(
        r#"
        const SW = ServiceWorker.prototype, SWR = ServiceWorkerRegistration.prototype, SWC = ServiceWorkerContainer.prototype;
        const get = (p, n) => Object.getOwnPropertyDescriptor(p, n).get;
        const sync = (label, f) => { try { const v = f(); print(label + "=NO THROW:" + String(v)); } catch (e) { print(label + "=" + e.constructor.name + ": " + e.message); } };
        const prom = (label, f) => { let v; try { v = f(); } catch (e) { print(label + "=SYNC THROW: " + e.message); return null; }
          if (!(v instanceof Promise)) { print(label + "=NOT A PROMISE: " + String(v)); return null; }
          return v.then(() => print(label + "=RESOLVED"), (e) => print(label + "=" + e.constructor.name + ": " + e.message)); };
        sync("controller", () => get(SWC, "controller").call({}));
        sync("startMessages", () => SWC.startMessages.call({}));
        sync("scope", () => get(SWR, "scope").call({}));
        sync("installing", () => get(SWR, "installing").call({}));
        sync("scriptURL", () => get(SW, "scriptURL").call({}));
        sync("state", () => get(SW, "state").call({}));
        sync("postMessage", () => SW.postMessage.call({}, "x"));
        Promise.all([
          prom("ready", () => get(SWC, "ready").call({})),
          prom("register", () => SWC.register.call({}, "/sw.js")),
          prom("getRegistration", () => SWC.getRegistration.call({})),
          prom("getRegistrations", () => SWC.getRegistrations.call({})),
          prom("unregister", () => SWR.unregister.call({})),
          prom("update", () => SWR.update.call({})),
          // Arity is a REJECTION too, not a synchronous throw, on a promise-returning operation.
          prom("registerNoArgs", () => navigator.serviceWorker.register()),
        ]).then(() => print("done"));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "controller=TypeError: Illegal invocation",
            "startMessages=TypeError: Illegal invocation",
            "scope=TypeError: Illegal invocation",
            "installing=TypeError: Illegal invocation",
            "scriptURL=TypeError: Illegal invocation",
            "state=TypeError: Illegal invocation",
            "postMessage=TypeError: Illegal invocation",
            "ready=TypeError: Failed to read the 'ready' property from 'ServiceWorkerContainer': Illegal invocation",
            "register=TypeError: Failed to execute 'register' on 'ServiceWorkerContainer': Illegal invocation",
            "getRegistration=TypeError: Failed to execute 'getRegistration' on 'ServiceWorkerContainer': Illegal invocation",
            "getRegistrations=TypeError: Failed to execute 'getRegistrations' on 'ServiceWorkerContainer': Illegal invocation",
            "unregister=TypeError: Failed to execute 'unregister' on 'ServiceWorkerRegistration': Illegal invocation",
            "update=TypeError: Failed to execute 'update' on 'ServiceWorkerRegistration': Illegal invocation",
            "registerNoArgs=TypeError: Failed to execute 'register' on 'ServiceWorkerContainer': 1 argument required, but only 0 present.",
            "done",
        ]
    );
}

/// The `register()` round trip against a REAL ServiceWorker isolate: what the page is handed, what
/// the registration map answers, and what `unregister()` does to it.
///
/// `Object.getOwnPropertyNames` of both objects is `[]` — the per-instance state is in v8 privates,
/// as it was in the JS's `WeakMap`, and Chrome's is `[]` too.
#[tokio::test]
async fn register_hands_back_real_instances_and_the_map_answers_them() {
    let out = probe_registered(
        r#"
        navigator.serviceWorker.register('./sw.js').then(async (reg) => {
          const w = reg.installing;
          const lines = [
            "regTag=" + Object.prototype.toString.call(reg),
            "regInstance=" + (reg instanceof ServiceWorkerRegistration),
            "regOwn=" + JSON.stringify(Object.getOwnPropertyNames(reg)),
            "scope=" + reg.scope,
            "waiting=" + String(reg.waiting) + " active=" + String(reg.active),
            "swTag=" + Object.prototype.toString.call(w),
            "swInstance=" + (w instanceof ServiceWorker),
            "swOwn=" + JSON.stringify(Object.getOwnPropertyNames(w)),
            "scriptURL=" + w.scriptURL + " state=" + w.state,
            "getRegistration=" + ((await navigator.serviceWorker.getRegistration()) === reg),
            "getRegistrationMiss=" + String(await navigator.serviceWorker.getRegistration("/nope/")),
            "getRegistrations=" + (await navigator.serviceWorker.getRegistrations()).length,
            // Chrome's: `ready` resolves with the registration that covers the page (it
            // resolved `undefined` before any registration existed until then).
            "ready=" + ((await navigator.serviceWorker.ready) === reg),
            "unregister=" + (await reg.unregister()),
            "afterUnregister=" + String(await navigator.serviceWorker.getRegistration()),
            "update=" + String(await reg.update()),
          ];
          print("RESULT:" + lines.join("|"));
        }, (e) => print("RESULT:REJECTED " + e));
        "#,
    )
    .await;
    let result = out.iter().find(|l| l.starts_with("RESULT:")).cloned().unwrap_or_else(|| format!("NONE {out:?}"));
    let parts: Vec<&str> = result.trim_start_matches("RESULT:").split('|').collect();
    assert_eq!(
        parts,
        vec![
            "regTag=[object ServiceWorkerRegistration]",
            "regInstance=true",
            "regOwn=[]",
            "scope=/",
            "waiting=null active=null",
            "swTag=[object ServiceWorker]",
            "swInstance=true",
            "swOwn=[]",
            "scriptURL=./sw.js state=installing",
            "getRegistration=true",
            "getRegistrationMiss=undefined",
            "getRegistrations=1",
            "ready=true",
            "unregister=true",
            "afterUnregister=undefined",
            "update=undefined",
        ],
        "full output: {out:?}"
    );
}

/// A `Client.postMessage` from inside the ServiceWorker fires the page's
/// `navigator.serviceWorker.onmessage` EXACTLY ONCE.
///
/// It fired TWICE before this cutover: `host.js` read the handler off the container and CALLED it,
/// and then also dispatched the event — and the container's `onmessage` is the realm's event-handler
/// store, which `dispatchEvent` already runs. The count is the assertion, not the delivery.
#[tokio::test]
async fn a_client_message_fires_the_container_handler_once() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    let clients_worker = include_str!("fixtures/worker/clients-worker.js");
    e.eval(
        "worker-scripts",
        format!(
            "globalThis.__workerScripts = {{ {}: {} }};",
            serde_json::to_string("./clients.js").unwrap(),
            serde_json::to_string(clients_worker).unwrap()
        ),
    )
    .unwrap();
    e.run().await.unwrap();
    e.eval(
        "driver",
        // Both delivery routes are counted: the `onmessage` handler AND an addEventListener
        // subscriber. One message must produce exactly one of each.
        "globalThis.__h = 0; globalThis.__l = 0; \
         navigator.serviceWorker.onmessage = () => { globalThis.__h++; }; \
         navigator.serviceWorker.addEventListener('message', () => { globalThis.__l++; }); \
         navigator.serviceWorker.register('./clients.js'); \
         setTimeout(() => print('RESULT:handler=' + globalThis.__h + ' listener=' + globalThis.__l), 1500);"
            .to_string(),
    )
    .unwrap();
    let mut out = Vec::new();
    let start = std::time::Instant::now();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if out.iter().any(|l| l.starts_with("RESULT:")) {
            break;
        }
        if start.elapsed() > std::time::Duration::from_secs(20) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let result = out.iter().find(|l| l.starts_with("RESULT:")).cloned().unwrap_or_else(|| format!("NONE {out:?}"));
    assert_eq!(result, "RESULT:handler=1 listener=1", "full output: {out:?}");
}

/// THE SERVICEWORKER REALM'S OWN GLOBALS: `clients`, `registration` and `skipWaiting()`.
///
/// They were `SW_PRELUDE`, a Rust string literal in `core/korvus-runtime/src/worker.rs` prepended to
/// every ServiceWorker's source — three object literals and an arrow, defining Web API inside the
/// kernel (I9). They are `ServiceWorkerGlobalScope` table rows now, so this reads them back from
/// INSIDE a real SW isolate.
///
/// The same string also wrote `globalThis.oninstall/onactivate/onfetch/onmessage = null` over four
/// rows the installer had already mounted, which is why `handlers=` and `handlerRoundTrip=` are here:
/// with the string, all four were plain data properties and the realm's handler store was bypassed.
#[tokio::test]
async fn the_service_worker_scopes_own_globals_are_table_rows() {
    let probe = include_str!("fixtures/worker/sw-scope-probe.js");
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    e.eval(
        "worker-scripts",
        format!(
            "globalThis.__workerScripts = {{ {}: {} }};",
            serde_json::to_string("./scope-probe.js").unwrap(),
            serde_json::to_string(probe).unwrap()
        ),
    )
    .unwrap();
    e.run().await.unwrap();
    e.eval(
        "driver",
        "const bc = new BroadcastChannel('sw-status'); bc.onmessage = (ev) => print('RESULT:' + ev.data); \
         navigator.serviceWorker.register('./scope-probe.js');"
            .to_string(),
    )
    .unwrap();
    let mut out = Vec::new();
    let start = std::time::Instant::now();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if out.iter().any(|l| l.starts_with("RESULT:")) {
            break;
        }
        if start.elapsed() > std::time::Duration::from_secs(20) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let result = out.iter().find(|l| l.starts_with("RESULT:")).cloned().unwrap_or_else(|| format!("NONE {out:?}"));
    let parts: Vec<&str> = result.trim_start_matches("RESULT:").split('|').collect();
    assert_eq!(
        parts,
        vec![
            "skipWaitingNative=true",
            "skipWaitingLen=0",
            "skipWaitingResolves=undefined",
            "handlers=accessor,accessor,accessor,accessor",
            "handlerRoundTrip=true",
            "registrationTag=[object ServiceWorkerRegistration]",
            "registrationInstance=true",
            "registrationOwn=[]",
            "registrationScope=/",
            "registrationStates=null/null/null",
            "registrationUnregister=true",
            "registrationListener=true",
            "clientsNative=true,true,true",
            "clientsSame=true",
            "client=1:true:window:true",
            // `[Global]` mounts a derived scope's rows ON the global object; see the fixture.
            "ownKeys=true,true,true",
        ],
        "full output: {out:?}"
    );
}

/// THE SERVICEWORKER REALM'S EVENT CHAIN: `ExtendableEvent`, `FetchEvent`,
/// `InstallEvent`.
///
/// They were `SW_PRELUDE`, a JS string in `core/korvus-runtime/src/worker.rs` prepended to every
/// ServiceWorker's own source, which built `new globalThis.Event('fetch')` and then wrote `request`,
/// `respondWith` and `waitUntil` onto it as OWN properties — three instance tells on an object a
/// browser leaves owning `isTrusted` and nothing else, with an object literal standing in for a
/// `Request`, on interfaces that did not exist in the realm at all.
///
/// # Where the expectations come from
///
/// THE CAPTURE BUNDLE RECORDS THIS REALM NOT AT ALL. `surface.json` and `worker.surface.json` contain
/// neither `FetchEvent` nor `ExtendableEvent` — zero occurrences — and `oracle/FetchEvent.json` /
/// `oracle/ExtendableEvent.json` hold only `"throws": "ReferenceError"`, which is the rig evaluating
/// them in the PAGE realm and recording that the names are not there. That negative is obeyed (the
/// install is ServiceWorker-realm-only, and `worker_surface` proves a dedicated worker gained no
/// name); for the positive shape the authority is truth source #2, REAL CHROME.
///
/// Every line below was produced by running THIS VERY FIXTURE as a real ServiceWorker in
/// `google-chrome` 150.0.7871.46 headless, driven by Playwright (python 1.60.0) over a loopback HTTP
/// server, 2026-09-12 — not transcribed from a spec and not from webref (whose `FetchEvent` has no
/// `isReload`; Chrome 150 does, so the table does). The two lines this engine deliberately answers
/// differently are marked where they sit.
#[tokio::test]
async fn the_service_worker_realms_event_chain_is_tables() {
    let probe = include_str!("fixtures/worker/sw-event-probe.js");
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    e.eval(
        "worker-scripts",
        format!(
            "globalThis.__workerScripts = {{ {}: {} }};",
            serde_json::to_string("./event-probe.js").unwrap(),
            serde_json::to_string(probe).unwrap()
        ),
    )
    .unwrap();
    e.run().await.unwrap();
    e.eval(
        "driver",
        "const rb = new BroadcastChannel('sw-ready'); \
         rb.onmessage = () => { fetch('/data').then((r) => r.text()).then((b) => print('BODY:' + b)); }; \
         const bc = new BroadcastChannel('sw-status'); bc.onmessage = (ev) => print('RESULT:' + ev.data); \
         navigator.serviceWorker.register('./event-probe.js');"
            .to_string(),
    )
    .unwrap();
    let mut out = Vec::new();
    let start = std::time::Instant::now();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if out.iter().any(|l| l.starts_with("RESULT:")) && out.iter().any(|l| l.starts_with("BODY:")) {
            break;
        }
        if start.elapsed() > std::time::Duration::from_secs(20) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    // END TO END: the page's `fetch` really was answered by the worker's `respondWith`. The transport
    // (the two `korvus-sw-fetch*` BroadcastChannels) is untouched by this unit; what changed is that
    // the event it carries is a real `FetchEvent` built in Rust.
    assert!(out.iter().any(|l| l == "BODY:from-sw-event-probe"), "SW did not intercept the fetch: {out:?}");
    let result = out.iter().find(|l| l.starts_with("RESULT:")).cloned().unwrap_or_else(|| format!("NONE {out:?}"));
    let parts: Vec<&str> = result.trim_start_matches("RESULT:").split('|').collect();
    assert_eq!(
        parts,
        vec![
            // The LIFECYCLE. `install` is an `InstallEvent` and `activate` a bare `ExtendableEvent`
            // in Chrome 150 — the deleted string fired a plain `Event` for both — and each owns
            // `isTrusted` and nothing else.
            r#"install=InstallEvent:[object InstallEvent]:["isTrusted"]:trusted=true:cancelable=false:bubbles=false"#,
            // BEFORE `activate`: `waitUntil` is real. The deleted string's `ev.waitUntil = () => {}`
            // threw the promise away and fired `activate` in the same tick, so this line landed after
            // it (or never, the fixture having been torn down first).
            "installWaitDone",
            r#"activate=ExtendableEvent:[object ExtendableEvent]:["isTrusted"]:trusted=true:cancelable=false:bubbles=false"#,
            r#"fetch=FetchEvent:[object FetchEvent]:["isTrusted"]:trusted=true:cancelable=true:bubbles=false"#,
            "chain=FetchEvent>ExtendableEvent>Event>Object",
            // The member ORDER, Chrome's own, `constructor` last on all three.
            r#"ExtendableEvent=["waitUntil","constructor"]"#,
            "ExtendableEvent.ctor=ExtendableEvent/1/true/Event/ExtendableEvent",
            r#"FetchEvent=["request","clientId","resultingClientId","isReload","preloadResponse","handled","respondWith","constructor"]"#,
            "FetchEvent.ctor=FetchEvent/2/true/ExtendableEvent/FetchEvent",
            r#"InstallEvent=["addRoutes","constructor"]"#,
            "InstallEvent.ctor=InstallEvent/1/true/ExtendableEvent/InstallEvent",
            "waitUntilShape=data/enum/conf/native:1",
            "respondWithShape=data/enum/conf/native:1",
            "requestShape=accessor/enum/conf/native:0",
            // A REAL `Request` with no own property, where the string passed `{url, method:'GET'}`.
            "request=Request:true:GET:[]",
            "requestUrlEndsWith=true",
            // CHROME ANSWERS A UUID for `clientId` (e.g. "d24ce185-01d1-4f84-8b3b-46f8c8f51d80").
            // This engine has exactly one Client and it is named `korvus-client` — the same id
            // `clients.matchAll()` hands back, which `sw-scope-probe.js` already asserts. One name for
            // one client is the honest answer here; a random UUID would be a fabricated value.
            r#"ids=["korvus-client",""]:false"#,
            "promises=[object Promise][object Promise]",
            "foreignWait=TypeError: Illegal invocation",
            "foreignRespond=TypeError: Illegal invocation",
            "foreignGet=TypeError: Illegal invocation",
            "noArgWait=TypeError: Failed to execute 'waitUntil' on 'ExtendableEvent': 1 argument required, but only 0 present.",
            "ctorNoInit=TypeError: Failed to construct 'FetchEvent': 2 arguments required, but only 1 present.",
            "ctorNoRequest=TypeError: Failed to construct 'FetchEvent': Failed to read the 'request' property from 'FetchEventInit': Required member is undefined.",
            "ctorNoType=TypeError: Failed to construct 'ExtendableEvent': 1 argument required, but only 0 present.",
            "ctorCall=TypeError: Failed to construct 'FetchEvent': Please use the 'new' operator, this DOM object constructor cannot be called as a function.",
            // A SCRIPT-CONSTRUCTED event is NOT trusted and NOT cancelable (the init dict said
            // nothing), `waitUntil` refuses it, and `respondWith` accepts it TWICE. That last
            // asymmetry is Chrome's, measured, not a simplification.
            r#"synth=FetchEvent:[object FetchEvent]:["isTrusted"]:trusted=false:cancelable=false:bubbles=false"#,
            "synthWait=InvalidStateError: Failed to execute 'waitUntil' on 'ExtendableEvent': Can not call waitUntil on a script constructed ExtendableEvent.",
            "synthRespond=no-throw:ok",
            "synthRespondTwice=no-throw:ok",
            "instanceOf=true,true,true",
            "respondTwice=InvalidStateError: Failed to execute 'respondWith' on 'FetchEvent': respondWith() was already called.",
            "respondLate=InvalidStateError: Failed to execute 'respondWith' on 'FetchEvent': The event handler is already finished.",
            "waitLate=InvalidStateError: Failed to execute 'waitUntil' on 'ExtendableEvent': The event handler is already finished and no extend lifetime promises are outstanding.",
        ],
        "full output: {out:?}"
    );
}
