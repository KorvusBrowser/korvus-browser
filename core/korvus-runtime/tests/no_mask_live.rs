//! What the kernel's `BOOTSTRAP` tail does now, and the measurement that decided it.
//!
//! W4.f was asked to DELETE the `[native code]` masking sweep and the bridge-symbol filter, on the
//! grounds that nothing is left to mask once the bodies are Rust. One of the two could go; the other
//! could not, and the difference was measured rather than assumed.
//!
//! # The measurement
//!
//! With the sweep removed, this file's roster scan reported **44 functions that stop reporting
//! `[native code]`**, every one of them a JS body in `core/korvus-runtime/src/js/`:
//!
//! ```text
//! host.js          EventTarget, Worker, SharedWorker, ImageData, Blob, File, DOMException,
//!                  AbortController, AbstractRange, StaticRange, Request, Response, Range,
//!                  getSelection                                                        (14)
//! shared-globals.js  console.{log,info,warn,error,debug,trace,dir,dirxml,group,…}       (36 — the
//!                  whole object literal)
//! host.js          chrome.loadTimes, chrome.csi, chrome.app.{getDetails,getIsInstalled,
//!                  installState,runningState}                                           (5)
//! ```
//!
//! # What the roster is down to
//!
//! The five `chrome` names are gone: `window.chrome` is
//! `core/korvus-webapi/src/modules/chrome_object.rs` and its six functions are `build_fn` natives, so
//! they report `[native code]` by BEING native. THIRTY-NINE remain — the 14 host.js classes and the
//! 36-member `console` literal, minus the overlap already noted — and `chrome`/`chrome.app` stay in
//! the roster scan below on purpose: a surface that needs no mark still has to be SCANNED, or nothing
//! would notice it regressing. `chrome_object_live.rs` is the shape gate that goes with it.
//!
//! Those files are explicitly NOT this unit's seam — W4.b owns `EventTarget`, W4.c the
//! fetch/file/abort/worker family and `shared-globals.js`, W4.e `Range`, and a sibling unit is inside
//! `host.js` now. Deleting the sweep would have traded one mask for 44 live automation tells, and
//! "stealth never regresses" predates this goal. So the sweep stays until its roster is empty, and
//! `scripts/freeze.sh`'s `js_files_runtime` / `define_property_js` ratchets are what drive it there.
//!
//! # What DID go
//!
//! The bridge-symbol filter is Rust (`webapi::hide_bridge_symbol`) and needs no mask, because a
//! `build_fn` function reports `[native code]` by BEING one — which is exactly what the deleted
//! `KB.__markNative(gops, …)` line had to fake. `webapi::hide_deno` replaces the stash-and-delete
//! tail. The sweep's two bridge slots (`KB.__markNative`, `KB.__maskObjectFns`) are deleted: their
//! only readers were the WebAssembly streaming block and the bridge-symbol filter, both now Rust.

use engine::Engine;

async fn probe(script: &str) -> Vec<String> {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body><div id=\"d\">x</div></body></html>").unwrap();
    e.eval("t", script.to_string()).unwrap();
    e.run().await.unwrap();
    e.take_output()
}

/// THE PROPERTY THAT MUST NEVER REGRESS, whoever provides it. Every function reachable on
/// `globalThis` and on the nine host singletons reports `[native code]`, and a page's own function
/// still shows its SOURCE — a realm that lied about everything would be as detectable as one that
/// lied about nothing.
///
/// This is the scan that produced the 44-name roster in the module header when the sweep was taken
/// out. It stays green through the sweep's deletion: each of those 44 turns native as its unit lands.
#[tokio::test]
async fn every_function_on_the_global_surface_reports_native() {
    let out = probe(
        r#"
        const ts = Function.prototype.toString;
        const isNative = (f) => { try { return ts.call(f).includes("[native code]"); } catch (e) { return true; } };
        const bad = [];
        const scan = (obj, label) => {
          if (!obj) return;
          let names; try { names = Object.getOwnPropertyNames(obj); } catch (e) { return; }
          for (const k of names) {
            let d; try { d = Object.getOwnPropertyDescriptor(obj, k); } catch (e) { continue; }
            if (!d) continue;
            for (const [kind, f] of [["", d.value], ["get ", d.get], ["set ", d.set]]) {
              if (typeof f !== "function") continue;
              if (!isNative(f)) bad.push(label + "." + kind + k);
            }
          }
        };
        scan(globalThis, "window");
        const nav = navigator;
        const roster = [[nav, "navigator"], [screen, "screen"], [history, "history"],
          [performance, "performance"], [console, "console"], [location, "location"],
          [crypto, "crypto"], [localStorage, "localStorage"], [sessionStorage, "sessionStorage"],
          [globalThis.chrome, "chrome"], [globalThis.chrome && globalThis.chrome.app, "chrome.app"],
          [globalThis.chrome && globalThis.chrome.runtime, "chrome.runtime"],
          [nav && nav.permissions, "navigator.permissions"], [nav && nav.mediaDevices, "navigator.mediaDevices"],
          [nav && nav.connection, "navigator.connection"]];
        for (const [o, label] of roster) {
          scan(o, label);
          try { scan(Object.getPrototypeOf(o), label + ".[[proto]]"); } catch (e) {}
        }
        for (const k of ["getBattery", "getGamepads"]) {
          if (typeof nav[k] === "function" && !isNative(nav[k])) bad.push("navigator." + k);
        }
        print(JSON.stringify(bad));
        // A page's own function is NOT masked: page scripts run after the sweep, so their source is
        // readable, exactly as in a browser.
        // A `const`, not a `function` declaration: the latter would become a global property and the
        // scan above would (correctly) report it as a non-native function on `window`.
        const mine = function (a) { return a + 1; };
        print("page=" + ts.call(mine).includes("return a + 1"));
        print("toString=" + isNative(ts) + " " + ts.name + "/" + ts.length);
        "#,
    )
    .await;
    assert_eq!(out, vec!["[]", "page=true", "toString=true toString/0"]);
}

/// The bridge symbol stays hidden, and the two functions that hide it are native because they are
/// RUST — not because something marked them.
#[tokio::test]
async fn the_bridge_symbol_is_filtered_by_native_functions() {
    let out = probe(
        r#"
        const ts = Function.prototype.toString;
        const B = Symbol.for("korvus.bridge");
        print("symbols=" + Object.getOwnPropertySymbols(globalThis).includes(B));
        print("ownKeys=" + Reflect.ownKeys(globalThis).includes(B));
        print("native=" + ts.call(Object.getOwnPropertySymbols).includes("[native code]")
                   + " " + ts.call(Reflect.ownKeys).includes("[native code]"));
        print("names=" + Object.getOwnPropertySymbols.name + "/" + Object.getOwnPropertySymbols.length
                 + " " + Reflect.ownKeys.name + "/" + Reflect.ownKeys.length);
        // A native function owns no `prototype`/`arguments`/`caller`; a JS wrapper would.
        print("tell=" + ["prototype", "arguments", "caller"].some((k) =>
            Object.prototype.hasOwnProperty.call(Object.getOwnPropertySymbols, k)));
        // The descriptor flags the intrinsics carry are kept.
        const d = Object.getOwnPropertyDescriptor(Object, "getOwnPropertySymbols");
        print("flags=" + d.writable + " " + d.enumerable + " " + d.configurable);
        // Every OTHER object is untouched: a page's own symbol survives both listings.
        const o = {}; const s = Symbol("mine"); o[s] = 1; o.a = 2;
        print("other=" + (Object.getOwnPropertySymbols(o).length === 1) + " " + Reflect.ownKeys(o).length);
        // `Reflect.ownKeys` of a primitive still throws, as the intrinsic does.
        try { Reflect.ownKeys(1); print("primitive=no throw"); } catch (e) { print("primitive=" + e.constructor.name); }
        // Nothing korvus-shaped is visible by NAME either.
        print("named=" + Object.getOwnPropertyNames(globalThis).filter((k) => /korvus/i.test(k)).length);
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "symbols=false",
            "ownKeys=false",
            "native=true true",
            "names=getOwnPropertySymbols/1 ownKeys/1",
            "tell=false",
            "flags=true false true",
            "other=true 2",
            "primitive=TypeError",
            "named=0",
        ]
    );
}

/// `Deno` and `__bootstrap` are gone, and everything that used to reach through them still works.
#[tokio::test]
async fn the_deno_global_is_gone_and_the_engine_still_works() {
    let out = probe(
        r#"
        print("deno=" + typeof Deno + " bootstrap=" + typeof __bootstrap);
        print("denoOwn=" + Object.getOwnPropertyNames(globalThis).filter((k) => k === "Deno" || k === "__bootstrap").length);
        // An op-backed native body: `document.getElementById` reaches the arena.
        print("dom=" + document.getElementById("d").textContent);
        // A body that reaches ops through the STASHED table rather than through `Deno`.
        print("report=" + typeof reportError);
        // And the engine's own sink, which is the `print` row of the Window table.
        print("sink=alive");
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec!["deno=undefined bootstrap=undefined", "denoOwn=0", "dom=x", "report=function", "sink=alive"]
    );
}
