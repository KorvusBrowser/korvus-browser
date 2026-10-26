//! CDP EVALUATION — `Runtime.evaluate`, `Runtime.callFunctionOn`, and the remote-object table they
//! hand back.
//!
//! Moved out of `lib.rs` VERBATIM. Nothing here was edited in the move.
//!
//! Two ways a value crosses back to the protocol and they answer different questions. A BY-VALUE
//! result is `JSON.stringify`d through V8's own stringifier, which is total (a function or a symbol
//! answers "no value") where `serde_v8` panics on several result types. A REMOTE result keeps the
//! value alive in [`Engine::objects`] under an `objectId`, because that is the only way
//! `callFunctionOn` can be handed the same object again later.
//!
//! [`Engine::eval_value_settled`] is the one with a rule worth restating: a NON-PROMISE never touches
//! the event loop, because ~10,000 recorded behaviour cases go through this path and a stray pump
//! between two of them would let a page's timers run where they previously could not.

use crate::Engine;
use deno_core::PollEventLoopOptions;

/// A bigint literal's decimal digits (`-123`) as `v8::BigInt::new_from_words` takes them: the sign, and the magnitude
/// in 64-bit words, least significant first. `None` for anything but an optional `-` and decimal digits.
fn bigint_words(d: &str) -> Option<(bool, Vec<u64>)> {
    let (negative, digits) = match d.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, d),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut words: Vec<u64> = vec![0];
    for b in digits.bytes() {
        // words = words * 10 + digit
        let mut carry = u128::from(b - b'0');
        for w in words.iter_mut() {
            let x = u128::from(*w) * 10 + carry;
            *w = x as u64;
            carry = x >> 64;
        }
        if carry > 0 {
            words.push(carry as u64);
        }
    }
    Some((negative, words))
}

/// How long an awaited evaluation waits on the page's own pump for a promise deno's loop gave up on (see
/// `settle_awaited`): `korvus-cdp`'s own `awaitPromise` budget, so a direct caller never waits longer than a CDP one.
const AWAIT_SETTLE_MS: u64 = 15_000;

/// A CDP argument to `callFunctionOn`: a by-value JSON literal or a remote object reference.
pub enum CdpArg {
    Value(serde_json::Value),
    Object(u32),
    /// A value JSON cannot carry (`Runtime.CallArgument.unserializableValue`: `NaN`, `Infinity`, `-Infinity`,
    /// `-0`, a bigint as `123n`), or `undefined` -- an argument with no value at all, which is how a client's
    /// `undefined` arrives (`{}`). It used to arrive as `null`: Puppeteer's `waitForSelector` passes `visible:
    /// undefined`, and its check (`visible === undefined ? node : …`) then never returned the node.
    Unserializable(Option<String>),
}

/// An awaited evaluation begun without waiting ([`Engine::eval_remote_begin_in`]): answered now, or a pending promise's
/// handle for [`Engine::settled`].
pub enum Awaited {
    Ready(EvalOutcome),
    Pending(u32),
}

/// Result of a remote evaluation: either a serialized value or a stored object handle.
pub enum RemoteResult {
    Value(serde_json::Value),
    Object(u32),
}

/// One CDP evaluation's whole answer: the value, and — when the expression threw or the promise it
/// produced rejected — the exception that goes with it.
///
/// # The defect this replaces (goal item 22, defect 3)
///
/// A throwing `Runtime.evaluate` answered a JSON-RPC **error**:
///
/// ```text
/// {"error":{"code":-32000,"message":"execute_script failed: Error: boom\n    at cdp:Runtime.evaluate:1:7"}}
/// ```
///
/// Chrome answers a **result** carrying `exceptionDetails`. That one string is two defects. It is a
/// *correctness* bug first: a client that evaluates page script and reads `exceptionDetails` — which
/// is exactly what a CDP client such as Playwright does — got
/// nothing usable, and real Playwright reported a page `throw` as
/// `"Execution context was destroyed, most likely because of a navigation"` (measured). And the text
/// leaked an internal script name, `cdp:Runtime.evaluate`, which is the class W9.16 and W9.21 close
/// elsewhere.
///
/// # Chrome's shape, measured
///
/// `google-chrome --headless=new --remote-debugging-port=0`, 2026-09-12, Chrome 150.0.7871.46,
/// `Runtime.evaluate` of `throw new Error('boom')` on a page session:
///
/// ```text
/// {"result":{
///   "result":{"type":"object","subtype":"error","className":"Error",
///             "description":"Error: boom\n    at <anonymous>:1:7","objectId":"4892…1.1"},
///   "exceptionDetails":{
///     "exceptionId":1,"text":"Uncaught","lineNumber":0,"columnNumber":0,"scriptId":"5",
///     "stackTrace":{"callFrames":[
///        {"functionName":"","scriptId":"5","url":"","lineNumber":0,"columnNumber":6}]},
///     "exception":{"type":"object","subtype":"error","className":"Error",
///                  "description":"Error: boom\n    at <anonymous>:1:7","objectId":"4892…1.2"}}}}
/// ```
///
/// Six things in there are not what a reasonable person would have guessed, so they are recorded
/// rather than inferred:
///
///  1. **`exceptionDetails` has no `url` key at all.** `<anonymous>` appears only inside the
///     `description` string (which is the Error's own `stack`), and the call frames carry
///     `"url": ""` — the empty string, not `"<anonymous>"`. The brief's "script name `<anonymous>`"
///     is true of the stack TEXT and false of every field.
///  2. **The thrown value appears TWICE**, in `result` and in `exceptionDetails.exception`, with two
///     DIFFERENT `objectId`s (`…1.1` and `…1.2`).
///  3. **`returnByValue: true` does not apply to a synchronous throw**: the result is still the error
///     object with an `objectId`. It *is* honoured for a rejected promise, where the result becomes
///     `{"type":"object","value":{}}` while the exception keeps its id. Chrome is inconsistent with
///     itself here and both halves are measured.
///  4. `text` is the bare word `"Uncaught"` for a synchronous throw — of an Error, of a plain string,
///     of anything. For a rejection it is `"Uncaught (in promise) Error: rejected!"`, or just
///     `"Uncaught (in promise)"` when the rejection value is not an Error.
///  5. A **SyntaxError carries no `stackTrace`** at all, only `exception` and a position.
///  6. `lineNumber`/`columnNumber` are the THROW SITE, 0-based, and differ from the top call frame:
///     `(function outer(){ function inner(){ throw new TypeError('deep') } inner() })()` gives
///     details `0,37` (the `throw`) beside frames `inner@0,43 · outer@0,67 · @0,77`.
///
/// And the rejection case (`Promise.reject(new Error('rejected!'))`, `awaitPromise: true`):
///
/// ```text
/// {"result":{"type":"object","subtype":"error","className":"Error",
///            "description":"Error: rejected!\n    at <anonymous>:1:16","objectId":"…1.13"},
///  "exceptionDetails":{"exceptionId":8,"text":"Uncaught (in promise) Error: rejected!",
///    "lineNumber":1,"columnNumber":16,"scriptId":"12",
///    "stackTrace":{"callFrames":[{"functionName":"","scriptId":"12","url":"",
///                                 "lineNumber":0,"columnNumber":15}]},
///    "exception":{…,"objectId":"…1.14"}}}
/// ```
///
/// Note `lineNumber: 1, columnNumber: 16` beside a call frame at `0, 15`: for a **rejection** Chrome
/// reports the top frame's position PLUS ONE on both axes, where a synchronous throw is reported
/// 0-based. That reads like a bug in Chrome; it is reproduced, because the job of this layer is to
/// answer what Chrome answers. `Promise.reject('plain')` (no Error, so no captured stack) gives no
/// `stackTrace` and `0,0`, which is the same rule with an empty stack.
pub struct EvalOutcome {
    pub result: RemoteResult,
    /// `None` when nothing was thrown.
    pub exception: Option<ExceptionInfo>,
}

/// CDP `Runtime.ExceptionDetails`, minus the `exceptionId` — that is a per-connection counter the
/// protocol layer owns (`plugins/korvus-cdp`'s `ConnIds::next_exception_id`).
pub struct ExceptionInfo {
    pub text: String,
    pub line_number: u32,
    pub column_number: u32,
    pub script_id: Option<String>,
    pub stack: Option<Vec<CallFrame>>,
    /// The SECOND handle on the thrown value — Chrome answers two.
    pub value: RemoteResult,
    pub class_name: Option<String>,
    /// The thrown Error's own `stack` string, which is what Chrome puts in `description`.
    pub description: Option<String>,
    pub is_error: bool,
}

/// One `Runtime.CallFrame`.
pub struct CallFrame {
    pub function_name: String,
    pub script_id: String,
    /// The script's resource name. For `Runtime.evaluate` that is the EMPTY STRING in Chrome, which
    /// is why [`Engine::eval_remote_details`] compiles with an empty script origin.
    pub url: String,
    pub line_number: u32,
    pub column_number: u32,
}

/// A caught exception, read off V8 once, before any of it becomes protocol JSON.
struct RawThrow {
    value: deno_core::v8::Global<deno_core::v8::Value>,
    line: u32,
    column: u32,
    stack: Option<Vec<CallFrame>>,
    class_name: Option<String>,
    description: Option<String>,
    is_error: bool,
}

impl RawThrow {
    /// Read the parts of a thrown value Chrome puts in its `RemoteObject`: whether it is an Error,
    /// its constructor name, and its `stack` — the string Chrome answers as `description`.
    fn describe(
        scope: &mut deno_core::v8::PinScope,
        value: deno_core::v8::Local<deno_core::v8::Value>,
        line: u32,
        column: u32,
        stack: Option<Vec<CallFrame>>,
    ) -> Self {
        use deno_core::v8;
        let is_error = value.is_native_error();
        let (class_name, description) = match v8::Local::<v8::Object>::try_from(value) {
            Ok(obj) => {
                let class = obj.get_constructor_name().to_rust_string_lossy(scope);
                let desc = v8::String::new(scope, "stack")
                    .and_then(|k| obj.get(scope, k.into()))
                    .filter(|s| s.is_string())
                    .map(|s| s.to_rust_string_lossy(scope));
                (Some(class), desc)
            }
            Err(_) => (None, None),
        };
        RawThrow {
            value: v8::Global::new(scope, value),
            line,
            column,
            stack,
            class_name,
            description,
            is_error,
        }
    }
}

/// Everything a `TryCatch` that has caught can tell us, from the two handles it hands over.
///
/// It takes the handles rather than the `TryCatch` itself so that both call sites (`run_caught` and
/// `call_function_on`, whose try-catch scopes have different concrete types) share one reading of
/// V8's answer — the position rules below are subtle enough that two copies would drift.
fn raw_throw<'s>(
    scope: &mut deno_core::v8::PinScope<'s, '_>,
    message: Option<deno_core::v8::Local<'s, deno_core::v8::Message>>,
    exception: Option<deno_core::v8::Local<'s, deno_core::v8::Value>>,
) -> RawThrow {
    use deno_core::v8;
    // Chrome's exceptionDetails position is the MESSAGE's (the throw site), not the top stack
    // frame's -- the nested-throw measurement on `EvalOutcome` is what shows they differ.
    let (line, column) = match &message {
        // V8's message line is 1-based and CDP's is 0-based; its column already is 0-based.
        Some(m) => (
            m.get_line_number(scope).map(|l| l.saturating_sub(1) as u32).unwrap_or(0),
            m.get_start_column() as u32,
        ),
        None => (0, 0),
    };
    // The stack AT THE THROW, which is what Chrome's call frames are. A SyntaxError has none --
    // Chrome answers `exceptionDetails` with no `stackTrace` key at all, and so does this.
    let stack = message
        .and_then(|m| m.get_stack_trace(scope))
        .filter(|t| t.get_frame_count() > 0)
        .map(|t| call_frames(scope, t));
    let value = match exception {
        Some(v) => v,
        None => v8::undefined(scope).into(),
    };
    RawThrow::describe(scope, value, line, column, stack)
}

/// Chrome's `Runtime.CallFrame[]`, read off a V8 stack trace.
///
/// V8 counts lines and columns from 1 here and CDP counts from 0, which is the whole of the
/// arithmetic: Chrome's measured frame for `throw new Error('boom')` is `line 0, column 6` beside a
/// stack text reading `at <anonymous>:1:7`.
fn call_frames(
    scope: &mut deno_core::v8::PinScope,
    trace: deno_core::v8::Local<deno_core::v8::StackTrace>,
) -> Vec<CallFrame> {
    let mut frames = Vec::with_capacity(trace.get_frame_count());
    for i in 0..trace.get_frame_count() {
        let Some(f) = trace.get_frame(scope, i) else { continue };
        frames.push(CallFrame {
            function_name: f
                .get_function_name(scope)
                .map(|s| s.to_rust_string_lossy(scope))
                .unwrap_or_default(),
            script_id: f.get_script_id().to_string(),
            url: f
                .get_script_name_or_source_url(scope)
                .map(|s| s.to_rust_string_lossy(scope))
                .unwrap_or_default(),
            line_number: (f.get_line_number() as u32).saturating_sub(1),
            column_number: (f.get_column() as u32).saturating_sub(1),
        });
    }
    frames
}

impl Engine {
    // `eval_remote` and `eval_remote_awaited` were here (W9.22 deleted them). They took a script
    // NAME -- the `"cdp:Runtime.evaluate"` that leaked onto the wire -- and turned a throw into a
    // string, which is why a throwing evaluate could only ever be a JSON-RPC error. The pair below
    // replaces both and is the sole owner of CDP evaluation; leaving the old pair as unused `pub`
    // methods would have left a second way to do it, with the defect attached.

    /// CDP `Runtime.evaluate` — the whole answer, exception included. See [`EvalOutcome`] for the
    /// defect this replaces and for Chrome's measured shape.
    ///
    /// Two things it does that [`Engine::eval_remote`] could not:
    ///
    ///   * **It keeps the thrown value.** `execute_script` turns a throw into a `JsError` string and
    ///     the V8 value is gone, which is why a throwing evaluate could only ever be answered as a
    ///     JSON-RPC error. Compiling and running under a `TryCatch` keeps the exception as a live
    ///     handle, so it goes back as a `RemoteObject` with an `objectId` a client can then call
    ///     `Runtime.getProperties` on — as Chrome's is.
    ///   * **It names the script the way Chrome names it: not at all.** The old path passed
    ///     `"cdp:Runtime.evaluate"` as the script name, and that string landed in `Error.stack` and
    ///     went out on the wire. Chrome compiles `Runtime.evaluate` with an EMPTY resource name, and
    ///     V8 renders an empty name as `<anonymous>` — which is why Chrome's measured `description`
    ///     reads `"Error: boom\n    at <anonymous>:1:7"` while its call frames carry `"url": ""`.
    ///     A `//# sourceURL=` comment still overrides the call frames' `url`, because that is V8
    ///     doing it and not this layer.
    ///
    /// ONE RESIDUAL DIFFERENCE, LOCATED AND NOT THIS CRATE'S. With a `//# sourceURL` comment
    /// Chrome's `description` reads `"Error: boom\n    at custom-name.js:1:7"` and this engine's
    /// reads `<anonymous>` — `description` is the Error's own `stack`, and this build's V8 formats
    /// a sourceURL-carrying stack differently. It is not a CDP choice and it is visible to the PAGE
    /// REALM with no CDP involved: `(0,eval)("throw new Error('e')\n//# sourceURL=zz.js")` gives
    /// Chrome `at eval (zz.js:1:7)` and this engine `at eval (zz.js, <anonymous>:1:7)`. Measured
    /// both ways, 2026-09-12. It belongs with the `Error.stack` work (W9.21).
    pub fn eval_remote_details(&mut self, src: String, return_by_value: bool) -> EvalOutcome {
        self.eval_remote_details_in(None, src, return_by_value)
    }

    /// [`Engine::eval_remote_details`] in isolated world `world` (`None`: the page's own), as `Runtime.evaluate`
    /// with a `contextId` (see [`Engine::create_isolated_world`]).
    pub fn eval_remote_details_in(&mut self, world: Option<usize>, src: String, return_by_value: bool) -> EvalOutcome {
        match self.run_caught(src, world) {
            Ok(global) => EvalOutcome {
                result: self.remote_from_global(global, return_by_value),
                exception: None,
            },
            Err(raw) => self.thrown_outcome(raw, "Uncaught".to_string(), None),
        }
    }

    /// CDP `Runtime.evaluate` with `awaitPromise: true`.
    ///
    /// A rejected promise is a RESULT with `exceptionDetails` in Chrome, not an error, and its `text`
    /// differs from the synchronous case. The rejection VALUE is read straight off the promise this
    /// evaluated to, because deno_core's `resolve` future reports a rejection as a `JsError` string
    /// and drops the value — the same reason `eval_remote_details` cannot use `execute_script`.
    pub async fn eval_remote_awaited_details(
        &mut self,
        src: String,
        return_by_value: bool,
    ) -> EvalOutcome {
        self.eval_remote_awaited_details_in(None, src, return_by_value).await
    }

    /// [`Engine::eval_remote_awaited_details`] in isolated world `world` (`None`: the page's own).
    pub async fn eval_remote_awaited_details_in(
        &mut self,
        world: Option<usize>,
        src: String,
        return_by_value: bool,
    ) -> EvalOutcome {
        let global = match self.run_caught(src, world) {
            Ok(g) => g,
            Err(raw) => return self.thrown_outcome(raw, "Uncaught".to_string(), None),
        };
        self.settle_awaited(global, return_by_value).await
    }

    /// `awaitPromise: true`'s second half, for `Runtime.evaluate` and `Runtime.callFunctionOn` alike: a promise the
    /// call produced is driven to settlement on the event loop, its value or its rejection then answered as Chrome
    /// answers them; anything else is answered as it is.
    async fn settle_awaited(&mut self, global: deno_core::v8::Global<deno_core::v8::Value>, return_by_value: bool) -> EvalOutcome {
        use deno_core::v8;
        let is_promise = {
            let ctx = self.runtime.main_context();
            v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
            v8::Local::new(scope, &global).is_promise()
        };
        if !is_promise {
            // Chrome answers a non-promise unchanged under awaitPromise (measured: `42` → 42).
            return EvalOutcome {
                result: self.remote_from_global(global, return_by_value),
                exception: None,
            };
        }
        let promise = global.clone();
        let resolve = self.runtime.resolve(global);
        let mut waited = self.runtime.with_event_loop_promise(resolve, PollEventLoopOptions::default()).await;
        // deno's loop says DRAINED while this engine's own machinery -- a MutationObserver's delivery, the rendering
        // opportunity, the task queue -- still holds what settles the promise, and deno answers that as an error.
        // It was then answered as a REJECTION with no value: Puppeteer's `waitForSelector` awaits a promise that a
        // MutationObserver resolves with the element, and read `Uncaught (in promise)` (compat). A promise
        // that is not rejected is now waited for on the page's own pump, bounded as a CDP await is.
        if waited.is_err() {
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(AWAIT_SETTLE_MS);
            loop {
                let state = {
                    let ctx = self.runtime.main_context();
                    v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
                    let local = v8::Local::new(scope, &promise);
                    match v8::Local::<v8::Promise>::try_from(local) {
                        Ok(p) => match p.state() {
                            v8::PromiseState::Fulfilled => Some(Ok(v8::Global::new(scope, p.result(scope)))),
                            v8::PromiseState::Rejected => Some(Err(())),
                            v8::PromiseState::Pending => None,
                        },
                        Err(_) => Some(Err(())),
                    }
                };
                match state {
                    Some(Ok(value)) => {
                        waited = Ok(value);
                        break;
                    }
                    Some(Err(())) => break,
                    None if std::time::Instant::now() >= deadline => break,
                    None => {
                        let end = self.pump(std::time::Duration::from_millis(25)).await;
                        if matches!(end, Ok(crate::PumpEnd::Drained)) {
                            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        }
                    }
                }
            }
        }
        match waited {
            Ok(settled) => EvalOutcome {
                result: self.remote_from_global(settled, return_by_value),
                exception: None,
            },
            Err(_) => {
                let raw = self.rejection_of(promise);
                // Chrome's text for a rejection carries the rejection itself when it is an Error --
                // "Uncaught (in promise) Error: rejected!" -- and is bare otherwise (measured with
                // `Promise.reject('plain')`). The suffix is the first line of the Error's stack,
                // which is its `name: message`.
                let text = match (raw.is_error, raw.description.as_deref()) {
                    (true, Some(d)) => {
                        format!("Uncaught (in promise) {}", d.split('\n').next().unwrap_or(d))
                    }
                    _ => "Uncaught (in promise)".to_string(),
                };
                self.thrown_outcome(raw, text, Some(return_by_value))
            }
        }
    }

    /// `awaitPromise: true` for a caller that does not wait in place -- the CDP server, which goes on answering other
    /// commands meanwhile, as Chrome's renderer does: the evaluation run, and a promise it produced that is still
    /// pending kept as a handle ([`Awaited::Pending`]) to ask about with [`Engine::settled`]; anything else answered
    /// now. A call whose answer arrives only through another command -- a page awaiting a binding the client answers
    /// (`page.exposeFunction`) -- needs exactly that.
    pub fn eval_remote_begin_in(&mut self, world: Option<usize>, src: String, return_by_value: bool) -> Awaited {
        match self.run_caught(src, world) {
            Ok(g) => self.begin_awaited(g, return_by_value),
            Err(raw) => Awaited::Ready(self.thrown_outcome(raw, "Uncaught".to_string(), None)),
        }
    }

    /// [`Engine::eval_remote_begin_in`] for `Runtime.callFunctionOn`.
    pub fn call_function_on_begin_in(
        &mut self,
        world: Option<usize>,
        fn_decl: &str,
        this_id: Option<u32>,
        args: Vec<CdpArg>,
        return_by_value: bool,
    ) -> anyhow::Result<Awaited> {
        Ok(match self.call_function_global(world, fn_decl, this_id, args)? {
            Ok(g) => self.begin_awaited(g, return_by_value),
            Err(raw) => Awaited::Ready(self.thrown_outcome(raw, "Uncaught".to_string(), None)),
        })
    }

    fn begin_awaited(&mut self, global: deno_core::v8::Global<deno_core::v8::Value>, return_by_value: bool) -> Awaited {
        use deno_core::v8;
        let pending = {
            let ctx = self.runtime.main_context();
            v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
            let local = v8::Local::new(scope, &global);
            v8::Local::<v8::Promise>::try_from(local).is_ok_and(|p| p.state() == v8::PromiseState::Pending)
        };
        if !pending {
            let id = self.next_object;
            self.next_object += 1;
            self.objects.insert(id, global);
            return match self.settled(id, return_by_value) {
                Some(outcome) => Awaited::Ready(outcome),
                None => Awaited::Pending(id),
            };
        }
        let id = self.next_object;
        self.next_object += 1;
        self.objects.insert(id, global);
        Awaited::Pending(id)
    }

    /// What an [`Awaited::Pending`] handle came to: `None` while its promise is pending; else its value, or its
    /// rejection as Chrome's `exceptionDetails` (`settle_awaited`'s answer) -- and the handle is released. A value that
    /// is no promise is answered as it is.
    pub fn settled(&mut self, handle: u32, return_by_value: bool) -> Option<EvalOutcome> {
        use deno_core::v8;
        let global = self.objects.get(&handle)?.clone();
        let state = {
            let ctx = self.runtime.main_context();
            v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
            let local = v8::Local::new(scope, &global);
            match v8::Local::<v8::Promise>::try_from(local) {
                Ok(p) => match p.state() {
                    v8::PromiseState::Pending => return None,
                    v8::PromiseState::Fulfilled => Ok(v8::Global::new(scope, p.result(scope))),
                    v8::PromiseState::Rejected => Err(()),
                },
                Err(_) => Ok(global.clone()),
            }
        };
        self.objects.remove(&handle);
        Some(match state {
            Ok(value) => EvalOutcome { result: self.remote_from_global(value, return_by_value), exception: None },
            Err(()) => {
                let raw = self.rejection_of(global);
                let text = match (raw.is_error, raw.description.as_deref()) {
                    (true, Some(d)) => format!("Uncaught (in promise) {}", d.split('\n').next().unwrap_or(d)),
                    _ => "Uncaught (in promise)".to_string(),
                };
                self.thrown_outcome(raw, text, Some(return_by_value))
            }
        })
    }

    /// Compile and run `src` under a `TryCatch`, keeping whatever it threw -- in isolated world `world` when one is
    /// named, the page's own context otherwise.
    fn run_caught(
        &mut self,
        src: String,
        world: Option<usize>,
    ) -> Result<deno_core::v8::Global<deno_core::v8::Value>, RawThrow> {
        use deno_core::v8;
        let ctx = self.runtime.main_context();
        v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
        let scope: &mut v8::PinScope = scope;
        let ctx = match world.and_then(|w| webapi::devtools_realm_context(scope, w)) {
            Some(c) => c,
            None => scope.get_current_context(),
        };
        let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, ctx);
        v8::tc_scope!(let tc, scope);

        // THE EMPTY RESOURCE NAME IS THE POINT: it is what Chrome compiles `Runtime.evaluate` with,
        // and what makes V8 render `<anonymous>` instead of this engine's own script name.
        let Some(empty) = v8::String::new(tc, "") else {
            let undef: v8::Local<v8::Value> = v8::undefined(tc).into();
            return Err(RawThrow::describe(tc, undef, 0, 0, None));
        };
        let origin =
            v8::ScriptOrigin::new(tc, empty.into(), 0, 0, false, 0, None, false, false, false, None);
        let ran = v8::String::new(tc, &src)
            .and_then(|code| v8::Script::compile(tc, code, Some(&origin)))
            .and_then(|script| script.run(tc));
        if let Some(v) = ran {
            return Ok(v8::Global::new(tc, v));
        }
        let (m, e) = (tc.message(), tc.exception());
        Err(raw_throw(tc, m, e))
    }

    /// The value a settled-rejected promise rejected with, with its position and stack.
    fn rejection_of(&mut self, promise: deno_core::v8::Global<deno_core::v8::Value>) -> RawThrow {
        use deno_core::v8;
        let ctx = self.runtime.main_context();
        v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
        let scope: &mut v8::PinScope = scope;
        let local = v8::Local::new(scope, &promise);
        // Neither fallback can happen (the caller checked `is_promise` and `with_event_loop_promise`
        // only errors on a rejection), but a panic here would take the isolate down over a page's
        // failed promise, so both answer `undefined` instead.
        let Ok(p) = v8::Local::<v8::Promise>::try_from(local) else {
            return RawThrow::describe(scope, v8::undefined(scope).into(), 0, 0, None);
        };
        if p.state() != v8::PromiseState::Rejected {
            return RawThrow::describe(scope, v8::undefined(scope).into(), 0, 0, None);
        }
        p.mark_as_handled();
        let value = p.result(scope);
        // A rejection's stack is the one its Error captured when it was CONSTRUCTED; there is no
        // throw site and so no message to take a position from.
        let stack = v8::Exception::get_stack_trace(scope, value)
            .filter(|t| t.get_frame_count() > 0)
            .map(|t| call_frames(scope, t));
        // MEASURED CHROME QUIRK: for a rejection the top-level lineNumber/columnNumber are the top
        // frame's PLUS ONE on both axes (frame 0,15 -> details 1,16), where a synchronous throw is
        // reported 0-based. Reproduced because this layer answers what Chrome answers.
        let (line, column) = match stack.as_ref().and_then(|f| f.first()) {
            Some(f) => (f.line_number + 1, f.column_number + 1),
            None => (0, 0),
        };
        RawThrow::describe(scope, value, line, column, stack)
    }

    /// Turn a caught value into the CDP answer: the thrown value in `result` AND a second handle on
    /// it inside `exceptionDetails.exception`, which is what Chrome hands back.
    ///
    /// `by_value` is `Some` only on the promise-rejection path. Chrome honours `returnByValue` for a
    /// rejection (`result` becomes `{"type":"object","value":{}}`) but NOT for a synchronous throw,
    /// where the result stays the error object with an `objectId`. Both measured.
    fn thrown_outcome(
        &mut self,
        raw: RawThrow,
        text: String,
        by_value: Option<bool>,
    ) -> EvalOutcome {
        let result = self.remote_from_global(raw.value.clone(), by_value.unwrap_or(false));
        let second = self.remote_from_global(raw.value, false);
        EvalOutcome {
            result,
            exception: Some(ExceptionInfo {
                text,
                line_number: raw.line,
                column_number: raw.column,
                script_id: raw.stack.as_ref().and_then(|f| f.first()).map(|f| f.script_id.clone()),
                stack: raw.stack,
                value: second,
                class_name: raw.class_name,
                description: raw.description,
                is_error: raw.is_error,
            }),
        }
    }

    /// CDP `Runtime.releaseObject`: drop the stored handle `id`, so what it held can be collected. `false` when no
    /// such handle is stored (never issued, or released already).
    pub fn release_object(&mut self, id: u32) -> bool {
        self.objects.remove(&id).is_some()
    }

    /// Turn a (resolved) V8 value into a CDP remote result: by-value JSON, or -- for a non-primitive
    /// when the caller didn't request by-value -- a stored objectId. (Chrome returns primitives by
    /// value regardless; only objects get an objectId.)
    fn remote_from_global(
        &mut self,
        global: deno_core::v8::Global<deno_core::v8::Value>,
        return_by_value: bool,
    ) -> RemoteResult {
        if return_by_value {
            return RemoteResult::Value(self.stringify_global(&global));
        }
        let is_object = {
            use deno_core::v8;
            let ctx = self.runtime.main_context();
            v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
            let local = v8::Local::new(scope, &global);
            local.is_object()
        };
        if is_object {
            let id = self.next_object;
            self.next_object += 1;
            self.objects.insert(id, global);
            RemoteResult::Object(id)
        } else {
            RemoteResult::Value(self.stringify_global(&global))
        }
    }

    /// CDP `Runtime.callFunctionOn`: compile `fn_decl`, call it with `this` (a stored object or
    /// undefined) and `args` (by-value literals resolved via JSON, or stored object refs).
    ///
    /// A THROW IS A RESULT HERE TOO, and this is the path that matters most: `page.evaluate` in
    /// Playwright is a `Runtime.callFunctionOn`, not a `Runtime.evaluate`. It used to answer
    /// `"function call returned nothing / threw"` — a string that names nothing and that no client
    /// can read a JS error out of. Chrome answers exactly what it answers for `Runtime.evaluate`
    /// (measured, `function(){ throw new Error('cfo boom') }`):
    /// `{"result":{…,"subtype":"error","description":"Error: cfo boom\n    at <anonymous>:1:20"},
    ///   "exceptionDetails":{"text":"Uncaught",…}}`.
    ///
    /// The `Err` is reserved for what Chrome answers `-32000` to: an `objectId` this server never
    /// issued (`"Invalid remote object id"`, measured).
    pub fn call_function_on(
        &mut self,
        fn_decl: &str,
        this_id: Option<u32>,
        args: Vec<CdpArg>,
        return_by_value: bool,
    ) -> anyhow::Result<EvalOutcome> {
        self.call_function_on_in(None, fn_decl, this_id, args, return_by_value)
    }

    /// [`Engine::call_function_on`] in the world Chrome runs it in: an object's own -- the function is
    /// compiled in the context of the `objectId` it is called on, when that is an isolated world -- else the
    /// `executionContextId`'s, `world` (`None`: the page's own).
    pub fn call_function_on_in(
        &mut self,
        world: Option<usize>,
        fn_decl: &str,
        this_id: Option<u32>,
        args: Vec<CdpArg>,
        return_by_value: bool,
    ) -> anyhow::Result<EvalOutcome> {
        let called = self.call_function_global(world, fn_decl, this_id, args)?;
        Ok(match called {
            Ok(global) => EvalOutcome {
                result: self.remote_from_global(global, return_by_value),
                exception: None,
            },
            Err(raw) => self.thrown_outcome(raw, "Uncaught".to_string(), None),
        })
    }

    /// [`Engine::call_function_on_in`] with `awaitPromise: true`: a promise the function returns -- every
    /// `async` function's -- is awaited on the event loop and its settled value answered, its rejection as Chrome's
    /// `exceptionDetails`, exactly as `Runtime.evaluate` awaits one. Playwright's `fill`, `click` and every action
    /// check are `async` functions run this way: answered with the PROMISE (`{}` by value), `fill` typed nothing and
    /// `click` never clicked (Chrome 150 answered the same call `"needsinput"`, `chrome150-compat-traffic.log`).
    pub async fn call_function_on_awaited_in(
        &mut self,
        world: Option<usize>,
        fn_decl: &str,
        this_id: Option<u32>,
        args: Vec<CdpArg>,
        return_by_value: bool,
    ) -> anyhow::Result<EvalOutcome> {
        let called = self.call_function_global(world, fn_decl, this_id, args)?;
        Ok(match called {
            Ok(global) => self.settle_awaited(global, return_by_value).await,
            Err(raw) => self.thrown_outcome(raw, "Uncaught".to_string(), None),
        })
    }

    /// The call itself: its value, or what it threw. `Err` only for an `objectId` this engine never issued or a
    /// declaration that is not a function.
    fn call_function_global(
        &mut self,
        world: Option<usize>,
        fn_decl: &str,
        this_id: Option<u32>,
        args: Vec<CdpArg>,
    ) -> anyhow::Result<Result<deno_core::v8::Global<deno_core::v8::Value>, RawThrow>> {
        use deno_core::v8;

        // Pre-resolve object handles (clones) so we don't borrow self.objects inside the scope.
        let this_g = match this_id {
            Some(id) => Some(self.objects.get(&id).ok_or_else(|| anyhow::anyhow!("no object {id}"))?.clone()),
            None => None,
        };
        enum R {
            Obj(v8::Global<v8::Value>),
            Json(String),
            Unserializable(Option<String>),
        }
        let resolved: Vec<R> = args
            .into_iter()
            .map(|a| match a {
                CdpArg::Object(id) => self
                    .objects
                    .get(&id)
                    .map(|g| R::Obj(g.clone()))
                    .ok_or_else(|| anyhow::anyhow!("no object {id}")),
                CdpArg::Value(v) => Ok(R::Json(serde_json::to_string(&v).unwrap_or_else(|_| "null".into()))),
                CdpArg::Unserializable(u) => Ok(R::Unserializable(u)),
            })
            .collect::<anyhow::Result<_>>()?;

        let ctx = self.runtime.main_context();
        let called: Result<v8::Global<v8::Value>, RawThrow> = {
            v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
            let scope: &mut v8::PinScope = scope;
            // The world the call runs in: `this`'s when it was made in one, else the one named.
            let this_world = this_g.as_ref().and_then(|g| {
                let local = v8::Local::new(scope, g);
                let ctx = v8::Local::<v8::Object>::try_from(local).ok()?.get_creation_context(scope)?;
                webapi::devtools_realm_of(scope, ctx)
            });
            let ctx = match this_world.or(world).and_then(|w| webapi::devtools_realm_context(scope, w)) {
                Some(c) => c,
                None => scope.get_current_context(),
            };
            let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, ctx);
            v8::tc_scope!(let tc, scope);
            let src = format!("({fn_decl})");
            let code = v8::String::new(tc, &src).ok_or_else(|| anyhow::anyhow!("bad fn src"))?;
            let script = v8::Script::compile(tc, code, None)
                .ok_or_else(|| anyhow::anyhow!("compile failed"))?;
            let func_val = script.run(tc).ok_or_else(|| anyhow::anyhow!("fn eval failed"))?;
            let func = v8::Local::<v8::Function>::try_from(func_val)
                .map_err(|_| anyhow::anyhow!("not a function"))?;

            let recv: v8::Local<v8::Value> = match &this_g {
                Some(g) => v8::Local::new(tc, g),
                None => v8::undefined(tc).into(),
            };
            let mut argv: Vec<v8::Local<v8::Value>> = Vec::with_capacity(resolved.len());
            for r in &resolved {
                let local = match r {
                    R::Obj(g) => v8::Local::new(tc, g),
                    R::Json(s) => {
                        let js = v8::String::new(tc, s).ok_or_else(|| anyhow::anyhow!("bad arg"))?;
                        v8::json::parse(tc, js).ok_or_else(|| anyhow::anyhow!("arg parse failed"))?
                    }
                    R::Unserializable(None) => v8::undefined(tc).into(),
                    R::Unserializable(Some(u)) => match u.as_str() {
                        "NaN" => v8::Number::new(tc, f64::NAN).into(),
                        "Infinity" => v8::Number::new(tc, f64::INFINITY).into(),
                        "-Infinity" => v8::Number::new(tc, f64::NEG_INFINITY).into(),
                        "-0" => v8::Number::new(tc, -0.0).into(),
                        big => match big.strip_suffix('n').and_then(bigint_words) {
                            // any size, as Chrome takes one (`123456789012345678901234567890n`)
                            Some((negative, words)) => v8::BigInt::new_from_words(tc, negative, &words)
                                .ok_or_else(|| anyhow::anyhow!("Invalid unserializable value"))?
                                .into(),
                            None => anyhow::bail!("Invalid unserializable value"),
                        },
                    },
                };
                argv.push(local);
            }
            match func.call(tc, recv, &argv) {
                Some(out) => Ok(v8::Global::new(tc, out)),
                None => {
                    let (m, e) = (tc.message(), tc.exception());
                    Err(raw_throw(tc, m, e))
                }
            }
        };
        Ok(called)
    }

    /// V8 `JSON.stringify` of a stored global → `serde_json::Value` (undefined/functions → Null).
    fn stringify_global(
        &mut self,
        global: &deno_core::v8::Global<deno_core::v8::Value>,
    ) -> serde_json::Value {
        use deno_core::v8;
        let ctx = self.runtime.main_context();
        v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
        let local = v8::Local::new(scope, global);
        match v8::json::stringify(scope, local) {
            Some(s) => serde_json::from_str(&s.to_rust_string_lossy(scope)).unwrap_or(serde_json::Value::Null),
            None => serde_json::Value::Null,
        }
    }

    /// Execute a script and return its completion value as JSON (for CDP `Runtime.evaluate`).
    /// Does not drain the event loop — call [`Engine::run`] first if the value depends on it.
    pub fn eval_value(
        &mut self,
        name: &'static str,
        src: String,
    ) -> anyhow::Result<serde_json::Value> {
        use deno_core::v8;
        let global = self
            .runtime
            .execute_script(name, src)
            .map_err(|e| anyhow::anyhow!("execute_script failed: {e}"))?;
        let ctx = self.runtime.main_context();
        v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
        let local = v8::Local::new(scope, global);
        // Convert via V8's own JSON.stringify — robust for any value (undefined/functions/symbols
        // yield None), unlike serde_v8 which panics on some result types.
        match v8::json::stringify(scope, local) {
            Some(s) => {
                let json = s.to_rust_string_lossy(scope);
                Ok(serde_json::from_str(&json).unwrap_or(serde_json::Value::Null))
            }
            None => Ok(serde_json::Value::Null),
        }
    }

    /// ABI 3 (WebExtension MV3 background, `host->spawn_background`): spawn a background realm for `src` at `url`.
    /// `kind` is "service" (a ServiceWorkerGlobalScope realm) -- any other kind returns 0. Runs at a page-loop task
    /// boundary (the `page_eval` pump), so it has the page's `op_state` (persona, location) that the spawn reads;
    /// it registers the SW isolate through the SAME path as `navigator.serviceWorker.register`. Returns the worker
    /// id (> 0) or 0 (unsupported kind). The realm then runs `src` on its own isolate + thread + event loop.
    pub fn spawn_background(&mut self, kind: &str, url: &str, src: &str) -> u32 {
        if kind != "service" {
            return 0;
        }
        use deno_core::v8;
        let ctx = self.runtime.main_context();
        v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
        let (id, is_new) = crate::worker::register_service_worker(scope, src.to_string(), url.to_string());
        // Arm the main-side reply pumps for a NEWLY-spawned SW. The navigator.serviceWorker.register path arms
        // these via start_isolate_pumps; the plugin-spawn path must too, or the background SW's BroadcastChannel
        // reply (the content<->background message bus, Gap B) sits in bc_outbound with nothing to drain it onto
        // the page isolate. Idempotent (PumpedIsolates), so a re-register / the register path is unaffected.
        if is_new && id > 0 {
            webapi::start_isolate_pumps(scope, id as f64);
        }
        id
    }

    /// Execute a script and return its completion value as JSON, first SETTLING it if it is a promise.
    ///
    /// [`Engine::eval_value`] answers with `{}` for a promise -- `JSON.stringify` of a Promise has no
    /// own enumerable properties -- which is indistinguishable from an empty object and is why the
    /// behavior oracle could never ask a question whose answer arrives asynchronously.
    ///
    /// Three properties this deliberately has:
    ///
    ///   * A NON-PROMISE never touches the event loop. The `is_promise` check keeps the synchronous
    ///     path byte-identical to `eval_value`, which matters because ~10,000 recorded behavior cases
    ///     go through it and a stray event-loop pump between two of them would let a page's timers run
    ///     where they previously could not.
    ///   * A promise that never settles is BOUNDED by `budget`, and the caller is told which. Without
    ///     it one hung member hangs the whole harness with nothing naming it.
    ///   * A REJECTION is an error, not a silent `null`. The behavior runner catches its own
    ///     rejections, so a rejection reaching here means the runner itself broke.
    pub async fn eval_value_settled(
        &mut self,
        name: &'static str,
        src: String,
        budget: std::time::Duration,
    ) -> anyhow::Result<serde_json::Value> {
        use deno_core::v8;
        let global = self
            .runtime
            .execute_script(name, src)
            .map_err(|e| anyhow::anyhow!("execute_script failed: {e}"))?;
        let is_promise = {
            let ctx = self.runtime.main_context();
            v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
            v8::Local::new(scope, &global).is_promise()
        };
        let settled = if is_promise {
            let resolve = self.runtime.resolve(global);
            let pumped = self
                .runtime
                .with_event_loop_promise(resolve, PollEventLoopOptions::default());
            match tokio::time::timeout(budget, pumped).await {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => return Err(anyhow::anyhow!("{name}: promise rejected: {e}")),
                Err(_) => {
                    return Err(anyhow::anyhow!("{name}: did not settle within {}ms", budget.as_millis()))
                }
            }
        } else {
            global
        };
        let ctx = self.runtime.main_context();
        v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
        let local = v8::Local::new(scope, settled);
        match v8::json::stringify(scope, local) {
            Some(s) => {
                let json = s.to_rust_string_lossy(scope);
                Ok(serde_json::from_str(&json).unwrap_or(serde_json::Value::Null))
            }
            None => Ok(serde_json::Value::Null),
        }
    }
}

/// The isolated worlds a document gets as it starts: the worlds its frame's DevTools client named,
/// each with the new-document scripts that name it, made before the document's first script runs -- Blink's
/// `InspectorPageAgent::DidCreateMainWorldContext` (`InjectScripts`) and the re-initialised window proxies of every
/// world the frame had (`LocalWindowProxyManager::UpdateDocument`).
/// One entry of the page's DevTools log (`webapi::devtools_log`), its values made remote results a CDP client can
/// inspect -- each stored as an evaluation's result is.
pub enum DevtoolsEntry {
    /// `Runtime.consoleAPICalled`.
    Console { kind: &'static str, args: Vec<RemoteResult>, world: Option<usize>, timestamp: f64 },
    /// `Runtime.exceptionThrown`.
    Error {
        text: String,
        url: String,
        line: u32,
        column: u32,
        value: Option<RemoteResult>,
        class_name: Option<String>,
        description: Option<String>,
        is_error: bool,
        timestamp: f64,
    },
}

impl Engine {
    /// What the page logged since the last call (see [`DevtoolsEntry`]). Empty while no client captures it.
    pub fn take_devtools_log(&mut self) -> Vec<DevtoolsEntry> {
        let entries = webapi::devtools_log::take();
        let mut out = Vec::with_capacity(entries.len());
        for e in entries {
            out.push(match e {
                webapi::devtools_log::Entry::Console(c) => DevtoolsEntry::Console {
                    kind: c.kind,
                    args: c.args.into_iter().map(|g| self.remote_from_global(g, false)).collect(),
                    world: c.world,
                    timestamp: c.timestamp,
                },
                webapi::devtools_log::Entry::Error(p) => DevtoolsEntry::Error {
                    text: p.text,
                    url: p.url,
                    line: p.line,
                    column: p.column,
                    value: p.value.map(|g| self.remote_from_global(g, false)),
                    class_name: p.class_name,
                    description: p.description,
                    is_error: p.is_error,
                    timestamp: p.timestamp,
                },
            });
        }
        out
    }

    /// `Runtime.addBinding`: `name` a global function of this document's main world now, and of every later
    /// document's (`webapi::devtools_log`).
    pub fn add_devtools_binding(&mut self, name: &str) {
        webapi::devtools_log::add_binding(name);
        let names = [name.to_string()];
        self.with_scope(|scope| webapi::devtools_log::install_bindings(scope, &names));
    }
}

#[derive(Default)]
pub(crate) struct DocumentWorlds {
    /// To make at the document's start, in order: a world's name and the scripts it runs there.
    pending: Vec<(String, Vec<String>)>,
    /// Made for this document: each world's name and index.
    made: Vec<(String, usize)>,
}

impl Engine {
    /// A new isolated world over the current document (`webapi::create_isolated_world`): its own global, intrinsics,
    /// interface objects and node wrappers, the page's DOM. Its index is what [`Engine::eval_remote_details_in`] and
    /// [`Engine::call_function_on_in`] name it by. `Page.createIsolatedWorld`.
    pub fn create_isolated_world(&mut self) -> usize {
        use deno_core::v8;
        let ctx = self.runtime.main_context();
        v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
        webapi::create_isolated_world(scope)
    }

    /// ABI 3 (WebExtension message bus, P1b): install the plugin bindings registered for named isolated world
    /// `world` (`host->add_world_binding`) into that world's realm, so a content script there can call them (e.g.
    /// chrome.runtime.sendMessage). Self-gating: a world no plugin bound has no registered names, so a CDP-named
    /// world or a no-webext run installs nothing -- byte-for-byte unchanged (I9/J2).
    fn install_bindings_in_world(&mut self, index: usize, world: &str) {
        let names = plugin_host::world_binding_names(world);
        if names.is_empty() {
            return;
        }
        use deno_core::v8;
        let ctx = self.runtime.main_context();
        v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
        if let Some(w) = webapi::isolated_world_context(scope, index) {
            let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, w);
            webapi::install_named_bindings(scope, &names);
        }
    }

    /// The worlds the NEXT document of this engine gets as it starts (see [`DocumentWorlds`]), in order: each name
    /// with the new-document scripts it runs. Read by the load, after the page-world new-document scripts and before
    /// the document's own.
    pub fn set_new_document_worlds(&mut self, worlds: Vec<(String, Vec<String>)>) {
        self.runtime.op_state().borrow_mut().put(DocumentWorlds { pending: worlds, made: Vec::new() });
    }

    /// APPEND named isolated worlds to the ones the current/next document gets as it starts (ABI 3, WebExtension
    /// B3), CREATING the [`DocumentWorlds`] resource when a load never set one (a non-CDP navigation). Unlike
    /// [`set_new_document_worlds`] it NEVER calls `put` -- it extends `pending` -- so the CDP-named worlds a
    /// navigation already staged keep their lower indices (and their CDP context ids) instead of being clobbered.
    /// Empty input is a no-op, so a loaded plugin that supplies no worlds leaves the resource untouched (which
    /// keeps `make_new_document_worlds`'s absent-resource early return intact on the non-CDP path); the call sites
    /// gate this on `plugin_host::any()`, so a no-plugin page never reaches it and is byte-for-byte unchanged (I9/J2).
    pub fn push_document_worlds(&mut self, worlds: Vec<(String, Vec<String>)>) {
        if worlds.is_empty() {
            return;
        }
        let st = self.runtime.op_state();
        let mut st = st.borrow_mut();
        if let Some(w) = st.try_borrow_mut::<DocumentWorlds>() {
            w.pending.extend(worlds); // append, after any CDP-staged worlds -> webext worlds get the higher indices
        } else {
            st.put(DocumentWorlds { pending: worlds, made: Vec::new() }); // create when absent (non-CDP load)
        }
    }

    /// The worlds the current document was given as it started: each name and its index.
    pub fn new_document_worlds(&self) -> Vec<(String, usize)> {
        self.runtime.op_state().borrow().try_borrow::<DocumentWorlds>().map(|w| w.made.clone()).unwrap_or_default()
    }

    /// Make the pending document worlds and run their scripts. A script that throws is not the page's error: Chrome
    /// runs it in its world and goes on.
    pub(crate) fn make_new_document_worlds(&mut self) {
        let pending = {
            let st = self.runtime.op_state();
            let mut st = st.borrow_mut();
            match st.try_borrow_mut::<DocumentWorlds>() {
                Some(w) => std::mem::take(&mut w.pending),
                None => return,
            }
        };
        let mut made = Vec::with_capacity(pending.len());
        for (name, scripts) in pending {
            let index = self.create_isolated_world();
            // ABI 3 (message bus): install this world's plugin bindings BEFORE its scripts run, so a content
            // script can call them. No-op for a world no plugin bound (self-gating), so CDP worlds are untouched.
            self.install_bindings_in_world(index, &name);
            for src in scripts {
                let _ = self.run_caught(src, Some(index));
            }
            made.push((name, index));
        }
        if let Some(w) = self.runtime.op_state().borrow_mut().try_borrow_mut::<DocumentWorlds>() {
            w.made = made;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::bigint_words;

    #[test]
    fn a_bigint_literal_of_any_size_becomes_its_words() {
        assert_eq!(bigint_words("0"), Some((false, vec![0])));
        assert_eq!(bigint_words("-5"), Some((true, vec![5])));
        assert_eq!(bigint_words("18446744073709551616"), Some((false, vec![0, 1])), "2^64");
        // 123456789012345678901234567890 = 6692605942 * 2^64 + 14083847773837265618
        assert_eq!(bigint_words("123456789012345678901234567890"), Some((false, vec![14083847773837265618, 6692605942])));
        assert_eq!(bigint_words(""), None);
        assert_eq!(bigint_words("12a"), None);
    }
}
