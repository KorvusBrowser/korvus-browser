//! The six WHATWG Streams interfaces.

use deno_core::v8;

use super::rows::{build_newable, parent_of};
use crate::install::installer::{self, Flavor};

/// The streams family from their `interface!` tables, in the WINDOW.
///
/// `ReadableStream`, `ReadableStreamDefaultReader`, `ReadableStreamDefaultController` (`core/streams/`),
/// `WritableStream`, `WritableStreamDefaultWriter` and `TransformStream` -- 28 rows that were 95 lines
/// of `dom/08g-observers.js`: 21 `op_*` builder round-trips, three `Object.defineProperty` sites, TWO
/// `op_nativize_ctor` masks (over the `WritableStream` and `TransformStream` JS constructor bodies),
/// one plain-JS `function ReadableStream(src)` on the global, and four `KB.__` bridge slots
/// (`__rsEnqueue`, `__rsClose`, `__wsInit`, `__tsInit`). All of that is deleted; this is their only
/// source.
///
/// Runs at context creation BESIDE [`super::install_window_small_elements`], BEFORE the bootstrap
/// script -- the position the deleted `dom/08g-observers.js` block ran from. Nothing in
/// `BOOTSTRAP` or `host.js` defines a
/// member of these six (grepped for all six names and for every member name): `host.js` only READS
/// `globalThis.ReadableStream` in its `enumerateProto` sweep, which needs the interface object to
/// exist by then, and it does.
///
/// The three NEWABLE ones (`ReadableStream`, `WritableStream`, `TransformStream` -- the capture records
/// `"newable": true` for exactly those three) get a real native constructor; the other three are the
/// installer's non-constructable interface objects, and every internal mint site re-prototypes onto the
/// table's prototype (`set_table_proto`) so no instance is left on the cppgc class template's.
pub fn install_window_streams<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    let mut ledger = installer::take_ledger(scope);
    install_streams_interfaces(scope, global, "korvus", &mut ledger);
    installer::put_ledger(scope, ledger);
}

/// The SAME six tables in the WORKER realm (invariant I8), called from
/// `core/korvus-runtime/src/worker.rs` beside [`super::install_worker_small_elements`].
///
/// The bundle's `worker.surface.json` lists all six with member lists byte-identical to the window's,
/// so this is the same call with the same tables -- a stream constructed in a worker answers exactly
/// what one constructed in the window does.
pub fn install_worker_streams<'s>(scope: &mut v8::PinScope<'s, '_>) {
    let global = scope.get_current_context().global(scope);
    let mut ledger = installer::take_ledger(scope);
    install_streams_interfaces(scope, global, "korvus-worker", &mut ledger);
    installer::put_ledger(scope, ledger);
}

fn install_streams_interfaces<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, realm: &str, ledger: &mut installer::InstallLedger) {
    use crate::core::streams::readable_stream::{cb_rs_ctor, install_rs_async_iterator, READABLESTREAM};
    use crate::core::streams::readable_stream_default_controller::READABLESTREAMDEFAULTCONTROLLER;
    use crate::core::streams::readable_stream_default_reader::READABLESTREAMDEFAULTREADER;
    use crate::core::streams::transform_stream::{cb_ts_ctor, TRANSFORMSTREAM};
    use crate::core::streams::writable_stream::{cb_ws_ctor, WRITABLESTREAM};
    use crate::core::streams::writable_stream_default_writer::WRITABLESTREAMDEFAULTWRITER;

    for def in [&READABLESTREAMDEFAULTREADER, &READABLESTREAMDEFAULTCONTROLLER, &WRITABLESTREAMDEFAULTWRITER] {
        // On the page, on a blueprint hit, put lazily instead (`installer::put_lazy_table`)
        if installer::put_lazy_table(scope, global, def, ledger) {
            continue;
        }
        let built = installer::create_interface(scope, global, def, def.name, None, None, Flavor::Table, ledger);
        for g in &built.gaps {
            eprintln!("[{realm}] streams install gap: {}.{}", g.interface, g.member);
        }
    }
    build_newable(scope, global, &READABLESTREAM, cb_rs_ctor, None, None, ledger);
    build_newable(scope, global, &WRITABLESTREAM, cb_ws_ctor, None, None, ledger);
    build_newable(scope, global, &TRANSFORMSTREAM, cb_ts_ctor, None, None, ledger);
    // `TextDecoderStream` / `TextEncoderStream` are NOT built here: they are created in
    // the registry walk at their capture position -- see `live::ORDERED_HAND`.
    // The one member no row can carry: see `install_rs_async_iterator`.
    if let Some((proto, _)) = parent_of(scope, "ReadableStream") {
        install_rs_async_iterator(scope, proto);
    }
}

/// `TextEncoderStream`, created at its capture position by the registry walk (`live::ORDERED_HAND`).
pub(crate) fn build_text_encoder_stream<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, ledger: &mut installer::InstallLedger) {
    use crate::core::streams::text_decoder_stream::{cb_tes_ctor, TEXTENCODERSTREAM};
    build_newable(scope, global, &TEXTENCODERSTREAM, cb_tes_ctor, None, None, ledger);
}

/// `TextDecoderStream`, created at its capture position by the registry walk (`live::ORDERED_HAND`).
pub(crate) fn build_text_decoder_stream<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, ledger: &mut installer::InstallLedger) {
    use crate::core::streams::text_decoder_stream::{cb_tds_ctor, TEXTDECODERSTREAM};
    build_newable(scope, global, &TEXTDECODERSTREAM, cb_tds_ctor, None, None, ledger);
}

/// `CompressionStream`, created at its capture position by the registry walk (`live::ORDERED_HAND`).
pub(crate) fn build_compression_stream<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, ledger: &mut installer::InstallLedger) {
    use crate::core::streams::compression_stream::{cb_cs_ctor, COMPRESSIONSTREAM};
    build_newable(scope, global, &COMPRESSIONSTREAM, cb_cs_ctor, None, None, ledger);
}

/// `DecompressionStream`, created at its capture position by the registry walk (`live::ORDERED_HAND`).
pub(crate) fn build_decompression_stream<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, ledger: &mut installer::InstallLedger) {
    use crate::core::streams::compression_stream::{cb_ds_ctor, DECOMPRESSIONSTREAM};
    build_newable(scope, global, &DECOMPRESSIONSTREAM, cb_ds_ctor, None, None, ledger);
}
