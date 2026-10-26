//! `icpt!`: a property interceptor callback, recorded in an analysis build.
//!
//! An interceptor configuration names its callbacks through this macro:
//!
//! ```ignore
//! .getter_raw(crate::binding::refs::c(icpt!(named_get, "Storage", cb_named_get).map_fn_to()))
//! ```
//!
//! Without the `analysis` feature it expands to the callback itself -- the same tokens, so a release
//! build is byte-for-byte what it was. With it, it expands to a wrapper that records the call as
//! `Iface.[[op]]` (the labels Chromium's recorder gives its generated interceptors), with the holder as
//! `this`, the property name or index (and the value, for a set) as arguments, and the return-value slot
//! as the result (a throw is read as the exception the callback left pending -- never caught, so it
//! propagates as without the recorder). The interface is a literal, or `holder` for a callback several interfaces share: then
//! it is the interface that DECLARES the getter for the holder's class ([`crate::analysis_hook::declaring_iface`]).
//!
//! Labels: named `[[get]]` `[[set]]` `[[delete]]` `[[has]]` `[[keys]]` `[[define]]`; indexed `[[index]]`
//! `[[index set]]` `[[index delete]]` `[[index has]]` `[[index keys]]` `[[index define]]`.

#[cfg(not(feature = "analysis"))]
macro_rules! icpt {
    ($kind:ident, $iface:tt, $cb:path) => {
        $cb
    };
}

#[cfg(feature = "analysis")]
macro_rules! icpt {
    (named_get, $iface:tt, $cb:path) => {{
        fn w<'s, 'i>(scope: &mut v8::PinScope<'s, 'i>, key: v8::Local<'s, v8::Name>, args: v8::PropertyCallbackArguments<'s>, rv: v8::ReturnValue<v8::Value>) -> v8::Intercepted {
            if !spec::analysis::enabled() || !$crate::analysis_hook::from_script(scope) {
                return $cb(scope, key, args, rv);
            }
            let holder = args.holder();
            let label = icpt!(@label scope, holder, $iface, "[[get]]", false);
            let a: [v8::Local<v8::Value>; 1] = [key.into()];
            let seq = $crate::analysis_hook::icpt_begin(scope, label, holder, &a);
            let out = rv;
            let r = $cb(scope, key, args, rv);
            let threw = $crate::analysis_hook::has_pending_exception(scope);
            let result = if threw { None } else { Some($crate::analysis_hook::RvValue::value(&out, scope)) };
            $crate::analysis_hook::icpt_end(scope, seq, result);
            r
        }
        w
    }};
    (named_set, $iface:tt, $cb:path) => {{
        fn w<'s, 'i>(scope: &mut v8::PinScope<'s, 'i>, key: v8::Local<'s, v8::Name>, value: v8::Local<'s, v8::Value>, args: v8::PropertyCallbackArguments<'s>, rv: v8::ReturnValue<v8::Boolean>) -> v8::Intercepted {
            if !spec::analysis::enabled() || !$crate::analysis_hook::from_script(scope) {
                return $cb(scope, key, value, args, rv);
            }
            let holder = args.holder();
            let label = icpt!(@label scope, holder, $iface, "[[set]]", false);
            let a: [v8::Local<v8::Value>; 2] = [key.into(), value];
            let seq = $crate::analysis_hook::icpt_begin(scope, label, holder, &a);
            let out = rv;
            let r = $cb(scope, key, value, args, rv);
            let threw = $crate::analysis_hook::has_pending_exception(scope);
            let result = if threw { None } else { Some($crate::analysis_hook::RvValue::value(&out, scope)) };
            $crate::analysis_hook::icpt_end(scope, seq, result);
            r
        }
        w
    }};
    (named_query, $iface:tt, $cb:path) => {{
        fn w<'s, 'i>(scope: &mut v8::PinScope<'s, 'i>, key: v8::Local<'s, v8::Name>, args: v8::PropertyCallbackArguments<'s>, rv: v8::ReturnValue<v8::Integer>) -> v8::Intercepted {
            if !spec::analysis::enabled() || !$crate::analysis_hook::from_script(scope) {
                return $cb(scope, key, args, rv);
            }
            let holder = args.holder();
            let label = icpt!(@label scope, holder, $iface, "[[has]]", false);
            let a: [v8::Local<v8::Value>; 1] = [key.into()];
            let seq = $crate::analysis_hook::icpt_begin(scope, label, holder, &a);
            let out = rv;
            let r = $cb(scope, key, args, rv);
            let threw = $crate::analysis_hook::has_pending_exception(scope);
            let result = if threw { None } else { Some($crate::analysis_hook::RvValue::value(&out, scope)) };
            $crate::analysis_hook::icpt_end(scope, seq, result);
            r
        }
        w
    }};
    (named_delete, $iface:tt, $cb:path) => {{
        fn w<'s, 'i>(scope: &mut v8::PinScope<'s, 'i>, key: v8::Local<'s, v8::Name>, args: v8::PropertyCallbackArguments<'s>, rv: v8::ReturnValue<v8::Boolean>) -> v8::Intercepted {
            if !spec::analysis::enabled() || !$crate::analysis_hook::from_script(scope) {
                return $cb(scope, key, args, rv);
            }
            let holder = args.holder();
            let label = icpt!(@label scope, holder, $iface, "[[delete]]", false);
            let a: [v8::Local<v8::Value>; 1] = [key.into()];
            let seq = $crate::analysis_hook::icpt_begin(scope, label, holder, &a);
            let out = rv;
            let r = $cb(scope, key, args, rv);
            let threw = $crate::analysis_hook::has_pending_exception(scope);
            let result = if threw { None } else { Some($crate::analysis_hook::RvValue::value(&out, scope)) };
            $crate::analysis_hook::icpt_end(scope, seq, result);
            r
        }
        w
    }};
    (named_define, $iface:tt, $cb:path) => {{
        fn w<'s, 'i>(scope: &mut v8::PinScope<'s, 'i>, key: v8::Local<'s, v8::Name>, desc: &v8::PropertyDescriptor, args: v8::PropertyCallbackArguments<'s>, rv: v8::ReturnValue<v8::Boolean>) -> v8::Intercepted {
            if !spec::analysis::enabled() || !$crate::analysis_hook::from_script(scope) {
                return $cb(scope, key, desc, args, rv);
            }
            let holder = args.holder();
            let label = icpt!(@label scope, holder, $iface, "[[define]]", false);
            let a: [v8::Local<v8::Value>; 1] = [key.into()];
            let seq = $crate::analysis_hook::icpt_begin(scope, label, holder, &a);
            let out = rv;
            let r = $cb(scope, key, desc, args, rv);
            let threw = $crate::analysis_hook::has_pending_exception(scope);
            let result = if threw { None } else { Some($crate::analysis_hook::RvValue::value(&out, scope)) };
            $crate::analysis_hook::icpt_end(scope, seq, result);
            r
        }
        w
    }};
    (named_keys, $iface:tt, $cb:path) => {{
        fn w<'s, 'i>(scope: &mut v8::PinScope<'s, 'i>, args: v8::PropertyCallbackArguments<'s>, rv: v8::ReturnValue<v8::Array>) {
            if !spec::analysis::enabled() || !$crate::analysis_hook::from_script(scope) {
                return $cb(scope, args, rv);
            }
            let holder = args.holder();
            let label = icpt!(@label scope, holder, $iface, "[[keys]]", false);
            let a: [v8::Local<v8::Value>; 0] = [];
            let seq = $crate::analysis_hook::icpt_begin(scope, label, holder, &a);
            let out = rv;
            let r = $cb(scope, args, rv);
            let threw = $crate::analysis_hook::has_pending_exception(scope);
            let result = if threw { None } else { Some($crate::analysis_hook::RvValue::value(&out, scope)) };
            $crate::analysis_hook::icpt_end(scope, seq, result);
            r
        }
        w
    }};
    (idx_get, $iface:tt, $cb:path) => {{
        fn w<'s, 'i>(scope: &mut v8::PinScope<'s, 'i>, index: u32, args: v8::PropertyCallbackArguments<'s>, rv: v8::ReturnValue<v8::Value>) -> v8::Intercepted {
            if !spec::analysis::enabled() || !$crate::analysis_hook::from_script(scope) {
                return $cb(scope, index, args, rv);
            }
            let holder = args.holder();
            let label = icpt!(@label scope, holder, $iface, "[[index]]", true);
            let a: [v8::Local<v8::Value>; 1] = [v8::Integer::new_from_unsigned(scope, index).into()];
            let seq = $crate::analysis_hook::icpt_begin(scope, label, holder, &a);
            let out = rv;
            let r = $cb(scope, index, args, rv);
            let threw = $crate::analysis_hook::has_pending_exception(scope);
            let result = if threw { None } else { Some($crate::analysis_hook::RvValue::value(&out, scope)) };
            $crate::analysis_hook::icpt_end(scope, seq, result);
            r
        }
        w
    }};
    (idx_set, $iface:tt, $cb:path) => {{
        fn w<'s, 'i>(scope: &mut v8::PinScope<'s, 'i>, index: u32, value: v8::Local<'s, v8::Value>, args: v8::PropertyCallbackArguments<'s>, rv: v8::ReturnValue<v8::Boolean>) -> v8::Intercepted {
            if !spec::analysis::enabled() || !$crate::analysis_hook::from_script(scope) {
                return $cb(scope, index, value, args, rv);
            }
            let holder = args.holder();
            let label = icpt!(@label scope, holder, $iface, "[[index set]]", true);
            let a: [v8::Local<v8::Value>; 2] = [v8::Integer::new_from_unsigned(scope, index).into(), value];
            let seq = $crate::analysis_hook::icpt_begin(scope, label, holder, &a);
            let out = rv;
            let r = $cb(scope, index, value, args, rv);
            let threw = $crate::analysis_hook::has_pending_exception(scope);
            let result = if threw { None } else { Some($crate::analysis_hook::RvValue::value(&out, scope)) };
            $crate::analysis_hook::icpt_end(scope, seq, result);
            r
        }
        w
    }};
    (idx_query, $iface:tt, $cb:path) => {{
        fn w<'s, 'i>(scope: &mut v8::PinScope<'s, 'i>, index: u32, args: v8::PropertyCallbackArguments<'s>, rv: v8::ReturnValue<v8::Integer>) -> v8::Intercepted {
            if !spec::analysis::enabled() || !$crate::analysis_hook::from_script(scope) {
                return $cb(scope, index, args, rv);
            }
            let holder = args.holder();
            let label = icpt!(@label scope, holder, $iface, "[[index has]]", true);
            let a: [v8::Local<v8::Value>; 1] = [v8::Integer::new_from_unsigned(scope, index).into()];
            let seq = $crate::analysis_hook::icpt_begin(scope, label, holder, &a);
            let out = rv;
            let r = $cb(scope, index, args, rv);
            let threw = $crate::analysis_hook::has_pending_exception(scope);
            let result = if threw { None } else { Some($crate::analysis_hook::RvValue::value(&out, scope)) };
            $crate::analysis_hook::icpt_end(scope, seq, result);
            r
        }
        w
    }};
    (idx_delete, $iface:tt, $cb:path) => {{
        fn w<'s, 'i>(scope: &mut v8::PinScope<'s, 'i>, index: u32, args: v8::PropertyCallbackArguments<'s>, rv: v8::ReturnValue<v8::Boolean>) -> v8::Intercepted {
            if !spec::analysis::enabled() || !$crate::analysis_hook::from_script(scope) {
                return $cb(scope, index, args, rv);
            }
            let holder = args.holder();
            let label = icpt!(@label scope, holder, $iface, "[[index delete]]", true);
            let a: [v8::Local<v8::Value>; 1] = [v8::Integer::new_from_unsigned(scope, index).into()];
            let seq = $crate::analysis_hook::icpt_begin(scope, label, holder, &a);
            let out = rv;
            let r = $cb(scope, index, args, rv);
            let threw = $crate::analysis_hook::has_pending_exception(scope);
            let result = if threw { None } else { Some($crate::analysis_hook::RvValue::value(&out, scope)) };
            $crate::analysis_hook::icpt_end(scope, seq, result);
            r
        }
        w
    }};
    (idx_define, $iface:tt, $cb:path) => {{
        fn w<'s, 'i>(scope: &mut v8::PinScope<'s, 'i>, index: u32, desc: &v8::PropertyDescriptor, args: v8::PropertyCallbackArguments<'s>, rv: v8::ReturnValue<v8::Boolean>) -> v8::Intercepted {
            if !spec::analysis::enabled() || !$crate::analysis_hook::from_script(scope) {
                return $cb(scope, index, desc, args, rv);
            }
            let holder = args.holder();
            let label = icpt!(@label scope, holder, $iface, "[[index define]]", true);
            let a: [v8::Local<v8::Value>; 1] = [v8::Integer::new_from_unsigned(scope, index).into()];
            let seq = $crate::analysis_hook::icpt_begin(scope, label, holder, &a);
            let out = rv;
            let r = $cb(scope, index, desc, args, rv);
            let threw = $crate::analysis_hook::has_pending_exception(scope);
            let result = if threw { None } else { Some($crate::analysis_hook::RvValue::value(&out, scope)) };
            $crate::analysis_hook::icpt_end(scope, seq, result);
            r
        }
        w
    }};
    (idx_keys, $iface:tt, $cb:path) => {{
        fn w<'s, 'i>(scope: &mut v8::PinScope<'s, 'i>, args: v8::PropertyCallbackArguments<'s>, rv: v8::ReturnValue<v8::Array>) {
            if !spec::analysis::enabled() || !$crate::analysis_hook::from_script(scope) {
                return $cb(scope, args, rv);
            }
            let holder = args.holder();
            let label = icpt!(@label scope, holder, $iface, "[[index keys]]", true);
            let a: [v8::Local<v8::Value>; 0] = [];
            let seq = $crate::analysis_hook::icpt_begin(scope, label, holder, &a);
            let out = rv;
            let r = $cb(scope, args, rv);
            let threw = $crate::analysis_hook::has_pending_exception(scope);
            let result = if threw { None } else { Some($crate::analysis_hook::RvValue::value(&out, scope)) };
            $crate::analysis_hook::icpt_end(scope, seq, result);
            r
        }
        w
    }};
    // the label: a literal interface's is made once; `holder`'s is looked up per call
    (@label $scope:ident, $holder:ident, holder, $op:literal, $indexed:expr) => {
        $crate::analysis_hook::interceptor_label($scope, $holder, $op, $indexed)
    };
    (@label $scope:ident, $holder:ident, $iface:literal, $op:literal, $indexed:expr) => {{
        static L: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
        *L.get_or_init(|| spec::analysis::intern($iface, $op))
    }};
}
