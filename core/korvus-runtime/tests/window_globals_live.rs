//! The window's self-aliases and its three host singletons, read back off the LIVE surface.
//!
//! `window`/`self`/`top`/`parent`/`frames` and `navigator`/`screen`/`location` left
//! `core/korvus-runtime/src/js/host.js` for `webapi`'s `core/html/window_globals.rs`. W6.2's finding
//! said they could not: the `deno_core` `objects =` classes are "reachable only from ESM". They are
//! not — they are realised onto `Deno.core.ops`, which is the same object `ext:core/ops` re-exports,
//! so Rust reaches the identical constructor. This file is the proof that the objects the Rust mints
//! are the same objects the JS minted: cppgc-backed, on the table's prototype, with the persona's
//! live values.
//!
//! It also pins the SEQUENCING CEILING that stopped the rest of the block moving, so that a later
//! unit that lifts it can see immediately whether it worked.

use engine::Engine;

async fn probe(script: &str) -> Vec<String> {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body>x</body></html>").unwrap();
    e.eval("t", script.to_string()).unwrap();
    e.run().await.unwrap();
    e.take_output()
}

/// The five aliases are the global itself, and each is the own ACCESSOR Chrome, Brave, Edge and Opera
/// all have (`fixtures/p11a/desc.*.txt`): `window` and `top` getter-only and
/// non-configurable (`[LegacyUnforgeable]`), `self`, `parent` and `frames` with a `[Replaceable]`
/// setter. This asserted the plain data property a JS assignment produced until the four were asked.
#[tokio::test]
async fn the_five_self_aliases_are_the_global() {
    let out = probe(
        r#"
        print("alias=" + ["window", "self", "top", "parent", "frames"]
            .map((n) => globalThis[n] === globalThis).join(","));
        print("desc=" + ["window", "self", "top", "parent", "frames"].map((n) => {
            const d = Object.getOwnPropertyDescriptor(globalThis, n);
            return d.get.name + "/" + (d.set ? d.set.name : "-") + "/" + d.enumerable + d.configurable;
        }).join(","));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "alias=true,true,true,true,true",
            "desc=get window/-/truefalse,get self/set self/truetrue,get top/-/truefalse,get parent/set parent/truetrue,get frames/set frames/truetrue"
        ]
    );
}

/// The three singletons are REAL instances of the deno_core classes — the cppgc unwrap that every
/// native getter does is what proves it, and a persona value read through one is the check that the
/// object Rust minted is the object the getters were built for.
///
/// `navigator.constructor` is the one-table `Navigator` interface object
/// (`install_window_navigator` re-points the instance's prototype AFTER this install, which is the
/// same relative order it had against `host.js`).
#[tokio::test]
async fn the_three_singletons_are_live_native_instances() {
    let out = probe(
        r#"
        print("types=" + [typeof navigator, typeof screen, typeof location].join(","));
        // A native getter on a cppgc class throws for a non-instance, so a real answer here is the
        // proof the Rust mint produced a genuine instance and not an `Object.create(proto)` impostor.
        print("persona=" + (typeof navigator.userAgent === "string" && navigator.userAgent.length > 0)
            + "," + (typeof screen.width === "number" && screen.width > 0)
            + "," + (typeof location.href === "string"));
        print("ctors=" + navigator.constructor.name + "," + screen.constructor.name + "," + location.constructor.name);
        print("tags=" + [navigator, screen, location].map((o) => Object.prototype.toString.call(o)).join(","));
        print("chain=" + (Object.getPrototypeOf(screen) === Screen.prototype));
        print("same=" + (navigator === navigator) + "," + (window.screen === screen));
        // Own ACCESSORS, as in every browser of the family: `navigator` getter-only,
        // `screen` `[Replaceable]`, `location` `[PutForwards=href]` and `[LegacyUnforgeable]`.
        print("desc=" + ["navigator", "screen", "location"].map((n) => {
            const d = Object.getOwnPropertyDescriptor(globalThis, n);
            return d.get.name + "/" + (d.set ? d.set.name : "-") + "/" + d.enumerable + d.configurable;
        }).join(","));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "types=object,object,object",
            "persona=true,true,true",
            "ctors=Navigator,Screen,Location",
            "tags=[object Navigator],[object Screen],[object Location]",
            "chain=true",
            "same=true,true",
            "desc=get navigator/-/truetrue,get screen/set screen/truetrue,get location/set location/truefalse",
        ]
    );
}

/// THE SEQUENCING CEILING, LIFTED — and what it was.
///
/// `Screen`, `TextEncoder` and `TextDecoder` were assigned from `host.js` when this test was
/// written, and the reason was measured rather than assumed: `installer::install_registry` SKIPS any
/// interface already on the global and `installer::backfill_members` only runs on one it FINDS
/// there, so a natively-realised class must be on the global BEFORE the registry install — and that
/// install was scheduled from JS (`generated_shells.js` called `op_install_registry()` during the ESM
/// layer), with no Rust hook ahead of it.
///
/// THE SECOND HALF STOPPED BEING TRUE IN A SIBLING LANE BEFORE IT WAS WRITTEN DOWN: W5.2 deleted
/// `core/korvus-webapi/src/js/` and with it that call site. The registry install is
/// `installer::install_window_registry`, a Rust call from the middle of
/// `install::live::bootstrap::install_window_bootstrap`, so anything at the TOP of that function is
/// ahead of it — which is where `bootstrap::install_window_host_classes` puts these three.
///
/// THIS TEST DID NOT CHANGE when they moved, which is the point of how it was written. Both
/// alternatives had been run on this tree (2026-09-12) and what they assert is the CONSEQUENCE:
///
///  1. delete the three `host.js` assignments, replace them with nothing → `screen instanceof
///     Screen` is FALSE and `new TextEncoder().encoding` is `""` (the registry minted a shell for
///     each name, and the deno_core class is unreachable from the page);
///  2. move them into `install_window_globals` instead, i.e. AFTER the registry install → the class
///     is back, and `Object.prototype.toString.call(screen)` becomes `[object EventTarget]`, because
///     the registry's `Symbol.toStringTag` backfill ran on the shell's prototype and
///     `JsScreen.prototype` (spliced onto `EventTarget.prototype`) inherits EventTarget's tag.
///
/// (2) is failure mode 4 of a table cutover — an install ORDER change discarding a decorator — and
/// `the_three_singletons_are_live_native_instances`'s `tags=` line is the half that catches it.
#[tokio::test]
async fn the_three_interface_objects_are_the_native_classes_not_registry_shells() {
    let out = probe(
        r#"
        // A registry shell's prototype carries D2 stubs; the native class's carries real accessors,
        // and `screen` is an instance of it.
        print("screen=" + (screen instanceof Screen));
        print("encoder=" + (new TextEncoder() instanceof TextEncoder) + "," + new TextEncoder().encoding);
        print("decoder=" + (new TextDecoder() instanceof TextDecoder) + "," + new TextDecoder().encoding);
        // The `.name` renames: deno_core names the class after the Rust ident (`JsScreen`).
        print("names=" + Screen.name + "," + TextEncoder.name + "," + TextDecoder.name);
        // Screen extends EventTarget in Chrome; the op2 class is standalone, so host.js splices it.
        print("screenChain=" + (Object.getPrototypeOf(Screen.prototype) === EventTarget.prototype));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "screen=true",
            "encoder=true,utf-8",
            "decoder=true,utf-8",
            "names=Screen,TextEncoder,TextDecoder",
            "screenChain=true",
        ]
    );
}

/// `frameElement` and `opener` are the `Window` TABLE's rows, and nothing writes a plain value over
/// them any more.
///
/// `host.js` wrote `globalThis.frameElement = null` / `globalThis.opener = null` at ESM time and the
/// table's install replaced both with accessors later in the same boot — a second writer of a slot
/// whose whole effect was already being undone. Both lines are deleted; these descriptors are the
/// proof that deleting them changed nothing a page can see.
#[tokio::test]
async fn frame_element_and_opener_are_the_window_tables_rows() {
    let out = probe(
        r#"
        for (const n of ["frameElement", "opener"]) {
          const d = Object.getOwnPropertyDescriptor(globalThis, n);
          print(n + "=" + String(globalThis[n]) + " accessor=" + (typeof d.get === "function")
              + " native=" + String(d.get).includes("[native code]")
              + " setter=" + (typeof d.set === "function")
              + " flags=" + d.enumerable + d.configurable);
        }
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "frameElement=null accessor=true native=true setter=false flags=truetrue",
            "opener=null accessor=true native=true setter=true flags=truetrue",
        ]
    );
}
