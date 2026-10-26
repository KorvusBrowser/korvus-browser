//! Streams Web-API interfaces (the blueprint's `core/streams/` domain).
//!
//! One file per interface. `ReadableStream` is the first slice, extracted verbatim from bindings.rs
//! (STEP 1). Its struct + the `op_rs_*` ops are re-exported so `bindings.rs` (the `extension!`
//! `objects = [...]` / `ops = [...]` lists) keeps referring to them unqualified — the same wiring
//! `core::geometry` uses. `ReadableStreamDefaultReader` still lives in bindings.rs and moves later.

use deno_core::v8;

/// WHATWG `[[closedPromise]]` / `[[readyPromise]]` — the one promise slot a reader and a writer each
/// own, and the ONLY thing that tells a consumer a stream ended.
///
/// It is one type because the reader's `closed`, the writer's `closed` and the writer's `ready` are
/// one state machine with three instances, and Chrome 150 answers the same four questions about each
/// (`tests/fixtures/stream-closed-probe.chrome-150.txt`):
///
/// * the promise a consumer reads is **stable**: `r.closed === r.closed`;
/// * it settles ONCE, with `undefined` on close/cancel and with the stream's stored error on error;
/// * `releaseLock` **rejects a pending one in place** (identity kept) and **REPLACES an already
///   settled one** with a fresh rejected promise (identity changes -- measured `false` on both
///   halves, which is why this is two code paths and not one);
/// * a rejection is marked HANDLED, so a page that never attached a handler does not get an
///   unhandled-rejection report for a promise the platform rejected on its behalf. That is WHATWG's
///   own `Set … [[PromiseIsHandled]] to true` in `ReadableStreamError`,
///   `ReadableStreamDefaultReaderRelease` and `WritableStreamDefaultWriterEnsureClosedPromiseRejected`
///   -- and without it every released reader in a page would report one.
///
/// [`Closed::arm`] takes a scope, so the slot starts EMPTY and every mint site arms it (the cppgc
/// `construct()` that `#[webapi(base)]` calls has no scope to mint a promise with). An unarmed slot
/// is inert: nothing settles it and the getter answers `undefined`, which is the honest answer for a
/// reader that was never handed a stream.
pub(crate) struct Closed {
    resolver: std::cell::RefCell<Option<v8::Global<v8::PromiseResolver>>>,
    promise: std::cell::RefCell<Option<v8::Global<v8::Promise>>>,
    settled: std::cell::Cell<bool>,
}

impl Closed {
    pub(crate) fn empty() -> Closed {
        Closed {
            resolver: std::cell::RefCell::new(None),
            promise: std::cell::RefCell::new(None),
            settled: std::cell::Cell::new(false),
        }
    }

    /// Mint the promise this slot answers with. Called at every mint site of a reader or a writer.
    pub(crate) fn arm(&self, scope: &mut v8::PinScope) {
        let Some(resolver) = v8::PromiseResolver::new(scope) else { return };
        let promise = resolver.get_promise(scope);
        *self.promise.borrow_mut() = Some(v8::Global::new(scope, promise));
        *self.resolver.borrow_mut() = Some(v8::Global::new(scope, resolver));
        self.settled.set(false);
    }

    /// The promise the getter answers. `undefined` for a slot nobody armed.
    pub(crate) fn get<'s>(&self, scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Value> {
        match self.promise.borrow().as_ref() {
            Some(p) => v8::Local::new(scope, p).into(),
            None => v8::undefined(scope).into(),
        }
    }

    /// Fulfil with `undefined` — a stream that closed or was cancelled. A second settle is a no-op.
    pub(crate) fn resolve(&self, scope: &mut v8::PinScope) {
        if self.settled.get() {
            return;
        }
        let Some(r) = self.resolver.borrow().clone() else { return };
        self.settled.set(true);
        let r = v8::Local::new(scope, &r);
        let u: v8::Local<v8::Value> = v8::undefined(scope).into();
        r.resolve(scope, u);
    }

    /// Reject with the stream's stored error, MARKED HANDLED. A second settle is a no-op.
    pub(crate) fn reject(&self, scope: &mut v8::PinScope, reason: v8::Local<v8::Value>) {
        if self.settled.get() {
            return;
        }
        let Some(r) = self.resolver.borrow().clone() else { return };
        self.settled.set(true);
        let r = v8::Local::new(scope, &r);
        r.reject(scope, reason);
        let p = r.get_promise(scope);
        p.mark_as_handled();
    }

    /// WHATWG's generic release, and the one place identity is observable: a PENDING promise is
    /// rejected in place, an already-settled one is REPLACED by a fresh rejected promise. Both
    /// marked handled.
    pub(crate) fn reject_or_replace(&self, scope: &mut v8::PinScope, reason: v8::Local<v8::Value>) {
        if !self.settled.get() {
            self.reject(scope, reason);
            return;
        }
        let Some(resolver) = v8::PromiseResolver::new(scope) else { return };
        resolver.reject(scope, reason);
        let promise = resolver.get_promise(scope);
        promise.mark_as_handled();
        *self.promise.borrow_mut() = Some(v8::Global::new(scope, promise));
        *self.resolver.borrow_mut() = Some(v8::Global::new(scope, resolver));
        self.settled.set(true);
    }
}

/// `reject_or_replace` on EVERY slot a `releaseLock` settles, with ONE `TypeError(msg)`.
///
/// One error object, not one per slot: `streams/writable-streams/general.any.js` asserts
/// `writer.ready`'s rejection **is** `writer.closed`'s (`assert_equals(readyRejection,
/// closedRejection)`), and Chrome 150 agrees -- measured, `releaseLock rejects closed and ready with
/// the SAME object = true`. Two TypeErrors carrying the same sentence read identically in a log and
/// fail that assertion.
pub(crate) fn release_slots(scope: &mut v8::PinScope, msg: &str, slots: &[&Closed]) {
    let Some(m) = v8::String::new(scope, msg) else { return };
    let e = v8::Exception::type_error(scope, m);
    for slot in slots {
        slot.reject_or_replace(scope, e);
    }
}

pub(crate) mod readable_stream;

pub(crate) mod readable_stream_default_controller;

pub(crate) mod readable_stream_default_reader;

pub(crate) mod writable_stream;

pub(crate) mod writable_stream_default_writer;

pub(crate) mod transform_stream;

pub(crate) mod text_decoder_stream;

pub(crate) mod compression_stream;
