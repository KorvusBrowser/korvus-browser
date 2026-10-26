//! The CONSTRUCTOR BODY of a generated registry row — the one column an emitted `InterfaceDef`
//! cannot carry.
//!
//! # What this replaces, and why it is not the same shape
//!
//! Twelve interfaces used to get their constructor by having `binding::protos::make_constructable_
//! with[_data]` **swap `globalThis[name]` after `create_interface` had already mounted it**: build a
//! second function with a raw `v8::Function::builder`, copy the old one's `length`, point it at the
//! old prototype, and assign over the global. Everything the installer had put on the interface
//! OBJECT went with the object it replaced.
//!
//! That is not a theoretical loss. Measured on the release binary at `e1aa5070` and again at
//! `e394d00e`: `EventSource.CLOSED` answered `undefined`, and `Object.getOwnPropertyNames(EventSource)`
//! was `["length","name","prototype"]`, where both captures (window AND worker) record
//! `CONNECTING`/`OPEN`/`CLOSED` as enumerable, non-configurable `0`/`1`/`2`. The swap also dropped
//! W7b's `korvus.installedBy` brand off the prototype's `constructor`, which is why the install
//! ledger reported all sixteen of them as `clobber:unbranded` second writers.
//!
//! # The answer: the interface object is constructable FROM ITS ROW
//!
//! [`row_ctor`] is the join between a generated row and the Rust body its constructor has. The ONE
//! installer reads it at create time ([`crate::install::installer::create_interface_in`]), so the
//! interface object is built once, with the row's `length`, the row's prototype, the row's statics
//! and the row's brand — and nothing swaps it afterwards. It is the same move L20 made for members
//! when `verify_native` learned to FINISH a cppgc member from its row instead of having a second pass
//! rebuild it beside it (a verification since deleted: the classes became own classes, whose
//! rows are mounted like any other).
//!
//! # Why the binding lives here rather than in the row
//!
//! `InterfaceDef` is emitted by `tools/sync/gen_shells.py` into `generated_registry.rs`, and an
//! emitted static cannot name a Rust `fn` that lives in a domain module: `CtorDef::install` is an
//! `InstallFn` (member-shaped: `(scope, proto, &MemberDef)`) and carries the stub sentinel for every
//! generated row. Teaching the generator to emit a crate path would make the generator depend on a
//! scan of the Rust tree for a fact that has nothing to do with the capture — a second inventory, for
//! twelve rows.
//!
//! So the join is Rust, and it is ONE function, in the ONE installer's own module, rather than four
//! `install_*_ctors` passes in four domain files. Nothing here decides SHAPE: the `length`, the
//! `name`, the prototype, the statics and the exposure are all still the row's, and a name that is
//! not a generated row's name mounts nothing at all (`install_registry` never calls this for a name
//! it did not create). It decides one thing — which Rust callback runs on `new` — which is exactly
//! what a hand table's `= <fn>` says for a member.
//!
//! # Why some of the sixteen are NOT here
//!
//! `EventSource` is not, and that is the finding this file's measurement produced: a `Model::Value`
//! registry row is ALREADY constructable from its row (`create_interface_in` builds it over
//! `cb_shell_ctor` with `ConstructorBehavior::Allow`). The swap that "made it
//! constructable" only replaced a branded, statics-carrying, instance-branding constructor with an
//! unbranded no-op one. Deleting the call is the whole fix.
//!
//! `Image`, `Audio` and `Option` are not, for the opposite reason: they are legacy element FACTORIES
//! whose `.prototype` is another interface's, so they cannot be built until that interface exists,
//! and they already have their own installs (`install::live::html_image`, `core::html::legacy_
//! factories`). The `make_constructable` loop ran over them in the ESM layer and was overwritten by
//! those installs minutes later — dead work whose only lasting effect was the ledger verdict it left
//! behind at the first `audit_surface` pass.

use deno_core::v8;

/// The Rust body a generated row's constructor has, plus how its `data` is bound.
pub(crate) struct RowCtor {
    pub cb: v8::FunctionCallback,
    /// Bind the interface NAME as the constructor's `data`. Two of the bodies below are ONE callback
    /// serving several interfaces and dispatch on it (`cb_new_audio_node` over the two channel nodes,
    /// `cb_gpu_error_ctor` over the three concrete `GPUError`s), exactly as they did when
    /// `make_constructable_with_data` bound it -- the interface-object swap, since deleted, of
    /// which this field is the surviving half. `false` binds `undefined`.
    pub data_is_name: bool,
}

impl RowCtor {
    fn plain(cb: impl v8::MapFnTo<v8::FunctionCallback>) -> Self {
        RowCtor { cb: cb.map_fn_to(), data_is_name: false }
    }
    fn named(cb: impl v8::MapFnTo<v8::FunctionCallback>) -> Self {
        RowCtor { cb: cb.map_fn_to(), data_is_name: true }
    }
}

/// The constructor body for `iface`, if this engine has one for a row the generator emitted.
///
/// `None` — every other row — keeps the model's own answer: a `Model::Value` row is newable through
/// `cb_shell_ctor` and any other row throws, which is what `create_interface_in` has always built.
///
/// REALM-AGNOSTIC ON PURPOSE (goal invariant I8). The row declares its own exposure and the worker
/// registry walks the same rows; `OffscreenCanvas` and the three `GPUError`s are `[Exposed=*]` and the
/// WORKER capture records them with `newable: false` and the same `length`, which is what their body
/// gives and what `cb_shell_ctor` did not. None of these callbacks reads `OpState`: they answer from
/// their arguments and the instance's own privates, so they are as valid on a worker thread as on the
/// page's.
/// Whether [`row_ctor`] names a body for `iface`.
///
/// Public because `tests/installer_parity`'s row-vs-surface check has to ask it: its rule for what
/// `new X()` must do was derived from `Model` alone ("a `Model::Value` row mints an instance"), which
/// was a sound derivation for exactly as long as every generated row's constructor was
/// `cb_shell_ctor`. A row that carries a body answers to the body, and to the CAPTURE behind it --
/// the bundle records `newable: false` for `OffscreenCanvas`, `PeriodicWave`, `AudioWorkletNode`, the
/// two channel nodes and the three `GPUError`s, and headless Chrome 150.0.7871.46 throws for every
/// one of them (measured 2026-09-12). The shape the model still fixes is that the interface object
/// HAS a `[[Construct]]`, which is what the check asserts for these.
pub fn has_row_ctor(iface: &str) -> bool {
    row_ctor(iface).is_some()
}

pub(crate) fn row_ctor(iface: &str) -> Option<RowCtor> {
    use crate::core::html::html_canvas_element::cb_offscreen_canvas_ctor;
    use crate::core::html::webaudio::{cb_new_audio_node, cb_new_periodic_wave, cb_new_worklet_node};
    use crate::core::html::webgpu::cb_gpu_error_ctor;

    Some(match iface {
        // Web Audio, the six interfaces of the family that stay generated rows (their members are
        // all stubs, so there is nothing for a table to own -- see `core/html/webaudio.rs`).
        "ChannelMergerNode" | "ChannelSplitterNode" => RowCtor::named(cb_new_audio_node),
        "PeriodicWave" => RowCtor::plain(cb_new_periodic_wave),
        "AudioWorkletNode" => RowCtor::plain(cb_new_worklet_node),
        // Reached only through another object's accessor, so the interface object exists and throws.
        // `RowCtor::named` because the body needs its interface name: Chrome 150 answers
        // `new AudioWorklet()` with `Failed to construct 'AudioWorklet': Illegal constructor` and a
        // bare `AudioWorklet()` with `Illegal constructor` (measured, headless, over loopback,
        // 2026-09-12 -- both shapes, all four names).
        "AudioWorklet" | "AudioParamMap" | "AudioPlaybackStats" | "AudioSinkInfo" => {
            RowCtor::named(crate::binding::protos::cb_illegal_ctor)
        }
        // The three concrete `GPUError`s: each takes its message.
        "GPUValidationError" | "GPUOutOfMemoryError" | "GPUInternalError" => RowCtor::named(cb_gpu_error_ctor),
        // `new OffscreenCanvas(w, h)` has to record the dimensions, or `width`/`height` answer
        // `undefined` and the whole `OffscreenCanvasRenderingContext2D` interface is unreachable.
        "OffscreenCanvas" => RowCtor::plain(cb_offscreen_canvas_ctor),
        // `new WebSocket(url, protocols)` opens a connection (Blink's `DOMWebSocket::Create`); the
        // shell constructor minted a socket that never opened, failed or closed.
        "WebSocket" => RowCtor::plain(crate::modules::websocket::web_socket::cb_ws_ctor),
        // `new CloseEvent(type, init)` has to keep `code` / `reason` / `wasClean`, and it is what a
        // WebSocket's own `close` event is minted through.
        "CloseEvent" => RowCtor::plain(crate::core::events::close_event::cb_close_event_ctor),
        _ => return None,
    })
}
