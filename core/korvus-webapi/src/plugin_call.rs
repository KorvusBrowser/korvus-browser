//! The Web API side of the `webapi` plugin slot: a claimed member's call at its exit, as
//! [`plugin_host::CallCtx`] reads and answers it (goal 1020 step 2).
//!
//! A member a loaded plugin claims is built as a fused record whose trampoline is
//! [`crate::install::templates`]'s `cb_claimed`; once the member's own body has run, the trampoline hands the call to
//! [`plugin_host::on_call`] through a [`V8Call`]. A member no plugin claims is built exactly as before and never reaches
//! this file (invariant I7).
use deno_core::v8;

/// One call at its exit: its arguments and the return value its body set.
pub(crate) struct V8Call<'a, 's, 'i> {
    pub scope: &'a mut v8::PinScope<'s, 'i>,
    pub args: &'a v8::FunctionCallbackArguments<'s>,
    pub rv: v8::ReturnValue<'s>,
}

impl V8Call<'_, '_, '_> {
    fn current(&mut self) -> v8::Local<'_, v8::Value> {
        self.rv.get(self.scope)
    }

    /// The return value's bytes when it is (or holds, as `ImageData.data`) a typed array of `want` elements' kind.
    fn view_of(&mut self, float: bool) -> Option<(*mut u8, usize)> {
        let mut v = self.rv.get(self.scope);
        if !v.is_array_buffer_view() {
            let o = v8::Local::<v8::Object>::try_from(v).ok()?;
            let key = v8::String::new(self.scope, "data")?;
            v = o.get(self.scope, key.into())?;
        }
        let ok = if float { v.is_float32_array() } else { v.is_uint8_clamped_array() || v.is_uint8_array() };
        if !ok {
            return None;
        }
        let view = v8::Local::<v8::ArrayBufferView>::try_from(v).ok()?;
        let len = view.byte_length();
        let offset = view.byte_offset();
        let buf = view.buffer(self.scope)?;
        let base = buf.data()?.as_ptr() as *mut u8;
        // SAFETY: the view's bytes lie inside its buffer's backing store, which the return value keeps alive across
        // the plugin's callback.
        Some((unsafe { base.add(offset) }, len))
    }
}

impl plugin_host::CallCtx for V8Call<'_, '_, '_> {
    fn argc(&mut self) -> i32 {
        self.args.length()
    }

    fn arg_number(&mut self, index: i32) -> Option<f64> {
        let v = self.args.get(index);
        if v.is_number() {
            v.number_value(self.scope)
        } else {
            None
        }
    }

    fn arg_string(&mut self, index: i32) -> Option<String> {
        let v = self.args.get(index);
        v.is_string().then(|| v.to_rust_string_lossy(self.scope))
    }

    fn set_number(&mut self, v: f64) {
        self.rv.set_double(v);
    }

    fn set_bool(&mut self, v: bool) {
        self.rv.set_bool(v);
    }

    fn set_string(&mut self, v: &str) {
        if let Some(s) = v8::String::new(self.scope, v) {
            self.rv.set(s.into());
        }
    }

    fn set_json(&mut self, v: &str) {
        let Some(text) = v8::String::new(self.scope, v) else { return };
        let Some(value) = v8::json::parse(self.scope, text) else { return };
        // A member that answers a promise answers the plugin's value through one, resolved.
        if self.current().is_promise() {
            if let Some(r) = v8::PromiseResolver::new(self.scope) {
                r.resolve(self.scope, value);
                let p = r.get_promise(self.scope);
                self.rv.set(p.into());
            }
            return;
        }
        self.rv.set(value);
    }

    fn return_bytes(&mut self) -> Option<(*mut u8, usize)> {
        self.view_of(false)
    }

    fn return_floats(&mut self) -> Option<(*mut f32, usize)> {
        self.view_of(true).map(|(p, len)| (p as *mut f32, len / 4))
    }

    fn return_json(&mut self) -> Option<String> {
        // V8's own JSON.stringify, as `eval_value` uses it: robust for any value, and `undefined` / a function / a
        // symbol stringify to nothing (None), which is the right answer for "a result a tracer can write down". Read
        // the return value inline (as `view_of` does) so it is not tied to the `&mut self` borrow `current` gives it.
        let v = self.rv.get(self.scope);
        let s = v8::json::stringify(self.scope, v)?;
        Some(s.to_rust_string_lossy(self.scope))
    }

    fn stack(&mut self, depth: usize) -> Option<String> {
        let st = v8::StackTrace::current_stack_trace(self.scope, depth.max(1))?;
        let n = st.get_frame_count();
        if n == 0 {
            return None;
        }
        let mut out = String::new();
        for k in 0..n {
            let Some(f) = st.get_frame(self.scope, k) else { continue };
            let func = f.get_function_name(self.scope).map(|s| s.to_rust_string_lossy(self.scope)).unwrap_or_default();
            let url = f.get_script_name(self.scope).map(|s| s.to_rust_string_lossy(self.scope)).unwrap_or_default();
            let func = if func.is_empty() { "<anonymous>" } else { &func };
            out.push_str(&format!("{func} {url}:{}:{}\n", f.get_line_number(), f.get_column()));
        }
        (!out.is_empty()).then_some(out)
    }

    fn realm(&mut self) -> i32 {
        let global = self.scope.get_current_context().global(self.scope);
        let name = global.get_constructor_name().to_rust_string_lossy(self.scope);
        match name.as_str() {
            "Window" => plugin_host::REALM_PAGE,
            "DedicatedWorkerGlobalScope" => plugin_host::REALM_DEDICATED_WORKER,
            "SharedWorkerGlobalScope" => plugin_host::REALM_SHARED_WORKER,
            "ServiceWorkerGlobalScope" => plugin_host::REALM_SERVICE_WORKER,
            _ => plugin_host::REALM_OTHER,
        }
    }
}
