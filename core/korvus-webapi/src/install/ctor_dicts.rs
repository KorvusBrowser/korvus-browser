//! WebIDL dictionary conversion for the registry's shell constructors.
//!
//! `binding::protos::cb_shell_ctor` builds every registry interface that has no constructor of its
//! own, and it used to read its dictionary argument loosely, if at all: `new BlobEvent("x", {})`,
//! `new AudioData({})`, `new CountQueuingStrategy({})` and 42 more constructed an object where stock
//! Chrome 150 throws `TypeError`, because a `required` member is undefined -- and an interface-typed
//! member was never checked. The Chrome oracle ratchet listed all 45.
//!
//! The conversion here is WebIDL's "convert an ECMAScript value to a dictionary", driven by
//! [`generated_ctor_dicts`](super::generated_ctor_dicts) -- which `tools/sync/gen_ctor_dicts.py`
//! writes from Blink 150's IDL (webref's where Blink declares nothing). Members are read in the
//! order WebIDL reads them (ancestors first, each dictionary's members sorted), each `Get` happening
//! exactly once, and every message is Blink's. The result is a fresh null-prototype object holding
//! the CONVERTED values of the members that were present, which is what the constructor steps read
//! afterwards -- so a getter on the page's dictionary runs once, as in Chrome, and never again.

use deno_core::v8;

use crate::binding::protos::{get_prop, str_val, throw_type_error};

/// A dictionary member's type, as far as conversion needs it.
#[derive(Clone, Copy, Debug)]
pub enum DictTy {
    /// `any`, or a type the generator could not classify exactly: the value as given.
    Any,
    /// `object` / `record<K, V>`: any object.
    Object,
    Bool,
    Int { bits: u32, signed: bool },
    /// `double`/`float` (restricted: NaN and the infinities throw) or their `unrestricted` forms;
    /// `single` is `float` (rounded to f32, and named so in the message).
    Double { restricted: bool, single: bool },
    /// DOMString / USVString / ByteString.
    Str,
    /// A WebIDL enum: its name and values.
    Enum(&'static str, &'static [&'static str]),
    /// An interface or buffer type, or a union of them: the value must be one of these.
    Iface(&'static [&'static str]),
    /// A callback function.
    Callback,
    /// A callback interface (`NodeFilter`, `EventListener`): any object, a function included.
    CallbackInterface,
    /// `sequence<T>` / `FrozenArray<T>`: iterated (WebIDL "create a sequence from an iterable"),
    /// each element converted to `elem`; a FrozenArray comes out frozen.
    Seq { elem: &'static DictTy, frozen: bool },
    /// A nested dictionary.
    Dict(&'static str),
}

/// A member's IDL default value: after conversion, a member with a default is PRESENT holding it
/// (WebIDL), so `new FontFaceSetLoadEvent("x").fontfaces` is `[]`, not the getter's shape default.
#[derive(Clone, Copy, Debug)]
pub enum Dflt {
    Null,
    Bool(bool),
    Num(f64),
    Str(&'static str),
    EmptySeq,
    EmptyDict,
}

/// One argument of a shell constructor.
#[derive(Clone, Copy, Debug)]
pub struct CtorArg {
    pub name: &'static str,
    pub ty: DictTy,
    pub optional: bool,
    pub nullable: bool,
    pub default: Option<Dflt>,
    pub variadic: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct DictMember {
    pub name: &'static str,
    pub ty: DictTy,
    pub required: bool,
    pub nullable: bool,
    pub default: Option<Dflt>,
    /// The dictionary that declares the member -- an inherited member's is the ancestor, and Blink's messages name
    /// it (`new SharedWorker(u, {type: 'x'})`: "... from 'WorkerOptions'", not `SharedWorkerOptions`).
    pub owner: &'static str,
}

fn members_of(dict: &str) -> Option<&'static [DictMember]> {
    // the generator writes the table sorted by name
    let t = super::generated_ctor_dicts::DICTS;
    t.binary_search_by(|(n, _)| (*n).cmp(dict)).ok().map(|i| t[i].1)
}

/// WebIDL's conversion of `v` to the dictionary `dict`, for a hand-written binding: what
/// `cb_shell_ctor` does for the shells. `undefined` and `null` are the empty dictionary, any other non-object Blink's
/// TypeError; the members are read in WebIDL's order (the ancestors' first, each dictionary's sorted), each `Get`
/// made once and its value converted as it is read, so a getter's throw or a bad value ends the conversion there.
/// The answer is a fresh null-prototype object holding the converted members that were present and the IDL
/// defaults of those that were not, which the binding reads in place of the page's object. `prefix` is the
/// operation's own context, "Failed to construct 'Worker': " or "Failed to execute 'observe' on 'X': ".
///
/// A stock Chrome 150 sweep (`fixtures/dict-conversion`) found the hand-written readers this replaces reading 101
/// of 229 dictionaries in their own order, leaving members out, and carrying on past a getter's throw -- which
/// then never reached the page.
///
/// `dict` must be a string literal: `tools/sync/gen_ctor_dicts.py` finds the dictionaries these calls name in the
/// source and emits their members (run `scripts/sync.sh` after adding a call).
pub fn convert_dict<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'_, v8::Value>, dict: &str, prefix: &str) -> Result<v8::Local<'s, v8::Object>, ()> {
    debug_assert!(members_of(dict).is_some(), "`convert_dict` names {dict}, which generated_ctor_dicts.rs does not hold: run scripts/sync.sh");
    // the value as a handle of this scope, whatever scope the caller's argument came from
    let v = v8::Local::new(scope, v);
    convert(scope, v, dict, prefix)
}

/// The dictionary argument of `iface`'s constructor, if the IDL gives it one.
pub fn dict_arg(iface: &str) -> Option<(usize, &'static str)> {
    super::generated_ctor_dicts::CTOR_DICT_ARGS.iter().find(|(i, _, _)| *i == iface).map(|(_, idx, d)| (*idx, *d))
}

/// A shell constructor's arguments, converted.
pub struct Converted<'s> {
    /// The dictionary argument (its index and the converted null-prototype object), if there is one.
    pub dict: Option<(usize, v8::Local<'s, v8::Object>)>,
    /// Every other argument the call passed or defaulted, converted, by its IDL name.
    pub named: v8::Local<'s, v8::Object>,
}

/// `iface`'s constructor arguments converted in order, as WebIDL converts them (Blink 150's IDL):
/// `Ok(None)` when the generator recorded no single constructor for it, `Err(())` after throwing.
pub fn convert_ctor_args<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    iface: &str,
    args: &v8::FunctionCallbackArguments<'s>,
) -> Result<Option<Converted<'s>>, ()> {
    let Some(specs) = super::generated_ctor_dicts::CTOR_ARGS.iter().find(|(i, _)| *i == iface).map(|(_, a)| *a) else {
        return Ok(None);
    };
    let prefix = format!("Failed to construct '{iface}': ");
    let named = null_proto_object(scope);
    let mut dict = None;
    for (i, a) in specs.iter().enumerate() {
        if a.variadic {
            break;
        }
        let v = args.get(i as i32);
        let missing = (i as i32) >= args.length() || (a.optional && v.is_undefined());
        if let DictTy::Dict(d) = a.ty {
            let src = if missing { v8::undefined(scope).into() } else { v };
            let o = convert(scope, src, d, &prefix)?;
            if dict.is_none() {
                dict = Some((i, o));
            }
            continue;
        }
        let cv = if missing {
            match a.default {
                Some(d) => dflt_value(scope, d, a.ty, &prefix)?,
                None => continue,
            }
        } else if a.nullable && v.is_null_or_undefined() {
            v8::null(scope).into()
        } else {
            positional(scope, v, a.ty, i + 1, &prefix)?
        };
        if let Some(k) = crate::binding::protos::key_str(scope, a.name) {
            named.create_data_property(scope, k.into(), cv);
        }
    }
    Ok(Some(Converted { dict, named }))
}

/// A stub method's arguments (`METHOD_ARGS`), converted in order as Blink's binding converts them before the
/// operation runs (measured: a stub converted none, so `navigator.hid.requestDevice({})` resolved
/// where Chrome rejects for its missing `filters`, and `navigator.geolocation.getCurrentPosition(5)` returned): a
/// dictionary converted whole (absent: the empty one), any other argument given converted with the parameter named
/// where Blink names it, an integer's `[EnforceRange]` / `[Clamp]` honoured. `Ok(false)` when the table holds no single
/// declaration of `label`, `Err(())` after throwing.
pub fn convert_method_args<'s>(scope: &mut v8::PinScope<'s, '_>, label: &str, args: &v8::FunctionCallbackArguments<'_>, prefix: &str) -> Result<bool, ()> {
    let table = super::generated_ctor_dicts::METHOD_ARGS;
    let Ok(at) = table.binary_search_by(|(l, _)| (*l).cmp(label)) else { return Ok(false) };
    for (i, a) in table[at].1.iter().enumerate() {
        if a.variadic {
            break;
        }
        let v = v8::Local::new(scope, args.get(i as i32));
        let missing = (i as i32) >= args.length() || (a.optional && v.is_undefined());
        if let DictTy::Dict(d) = a.ty {
            let src = if missing { v8::undefined(scope).into() } else { v };
            convert(scope, src, d, prefix)?;
            continue;
        }
        if missing || (a.nullable && v.is_null_or_undefined()) {
            continue;
        }
        let range = super::generated_ctor_dicts::METHOD_ARG_RANGES.iter().find(|(l, n, _)| *l == label && *n == a.name).map(|(_, _, e)| *e);
        match (a.ty, range) {
            (DictTy::Int { bits, signed }, Some(enforce)) => {
                ranged_integer(scope, v, bits, signed, enforce, &prefix)?;
            }
            _ => {
                positional(scope, v, a.ty, i + 1, prefix)?;
            }
        }
    }
    Ok(true)
}

/// [`convert_method_args`], handing back what each argument converted to -- a dictionary argument's converted
/// null-prototype object, any other argument's converted value, `None` for one not given -- for a binding that runs
/// on the converted values (`navigation.navigate(url, options)`), so nothing is converted twice. `Ok(None)` when
/// the table holds no single declaration of `label`, `Err(())` after throwing.
pub fn convert_method_args_values<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    label: &str,
    args: &v8::FunctionCallbackArguments<'_>,
    prefix: &str,
) -> Result<Option<Vec<Option<v8::Local<'s, v8::Value>>>>, ()> {
    let table = super::generated_ctor_dicts::METHOD_ARGS;
    let Ok(at) = table.binary_search_by(|(l, _)| (*l).cmp(label)) else { return Ok(None) };
    let mut out = Vec::new();
    for (i, a) in table[at].1.iter().enumerate() {
        if a.variadic {
            break;
        }
        let v = v8::Local::new(scope, args.get(i as i32));
        let missing = (i as i32) >= args.length() || (a.optional && v.is_undefined());
        if let DictTy::Dict(d) = a.ty {
            let src = if missing { v8::undefined(scope).into() } else { v };
            let o = convert(scope, src, d, prefix)?;
            out.push(Some(o.into()));
            continue;
        }
        if missing {
            out.push(None);
            continue;
        }
        if a.nullable && v.is_null_or_undefined() {
            out.push(Some(v8::null(scope).into()));
            continue;
        }
        let range = super::generated_ctor_dicts::METHOD_ARG_RANGES.iter().find(|(l, n, _)| *l == label && *n == a.name).map(|(_, _, e)| *e);
        let cv = match (a.ty, range) {
            (DictTy::Int { bits, signed }, Some(enforce)) => ranged_integer(scope, v, bits, signed, enforce, &prefix)?,
            _ => positional(scope, v, a.ty, i + 1, prefix)?,
        };
        out.push(Some(cv));
    }
    Ok(Some(out))
}

/// [`convert_method_args`] for a PROMISE-returning operation, whose conversion failure is a rejection: the exception
/// is caught and handed back for the caller to reject with.
pub fn convert_method_args_caught<'s>(scope: &mut v8::PinScope<'s, '_>, label: &str, args: &v8::FunctionCallbackArguments<'_>, prefix: &str) -> Result<bool, v8::Local<'s, v8::Value>> {
    let r = {
        v8::tc_scope!(let tc, scope);
        match convert_method_args(tc, label, args, prefix) {
            Ok(b) => Ok(b),
            Err(()) => {
                let e = tc.exception().map(|e| v8::Global::new(tc, e));
                tc.reset();
                Err(e)
            }
        }
    };
    match r {
        Ok(b) => Ok(b),
        Err(Some(e)) => Err(v8::Local::new(scope, &e)),
        Err(None) => Err(v8::undefined(scope).into()),
    }
}

/// A positional argument: Blink names the PARAMETER when an object of the wrong interface, a
/// non-function or a non-object is passed; every other type converts as a dictionary member does.
fn positional<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    v: v8::Local<'s, v8::Value>,
    ty: DictTy,
    n: usize,
    prefix: &str,
) -> Result<v8::Local<'s, v8::Value>, ()> {
    let want = match ty {
        DictTy::Iface(names) if !names.iter().any(|x| is_a(scope, v, x)) => Some(names.join("' or '")),
        DictTy::Callback if !v.is_function() => Some("Function".to_string()),
        DictTy::CallbackInterface if !v.is_object() => Some("Object".to_string()),
        DictTy::Object if !v.is_object() => Some("object".to_string()),
        _ => None,
    };
    if let Some(w) = want {
        throw_type_error(scope, &format!("{prefix}parameter {n} is not of type '{w}'."));
        return Err(());
    }
    member(scope, v, ty, &prefix)
}

/// An IDL default value, materialised.
fn dflt_value<'s>(scope: &mut v8::PinScope<'s, '_>, d: Dflt, ty: DictTy, at: &dyn std::fmt::Display) -> Result<v8::Local<'s, v8::Value>, ()> {
    Ok(match d {
        Dflt::Null => v8::null(scope).into(),
        Dflt::Bool(b) => v8::Boolean::new(scope, b).into(),
        Dflt::Num(n) => v8::Number::new(scope, n).into(),
        Dflt::Str(t) => str_val(scope, t),
        Dflt::EmptySeq => {
            let a = v8::Array::new(scope, 0);
            if let DictTy::Seq { frozen: true, .. } = ty {
                let _ = a.set_integrity_level(scope, v8::IntegrityLevel::Frozen);
            }
            a.into()
        }
        Dflt::EmptyDict => match ty {
            DictTy::Dict(name) => {
                let u = v8::undefined(scope).into();
                convert(scope, u, name, &at.to_string())?.into()
            }
            _ => v8::Object::new(scope).into(),
        },
    })
}

/// [`convert_dict`] for a PROMISE-returning operation, whose conversion failure is a rejection, not a throw: the
/// exception the conversion threw is caught and handed back for the caller to reject with.
pub fn convert_dict_caught<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    v: v8::Local<'_, v8::Value>,
    dict: &str,
    prefix: &str,
) -> Result<v8::Local<'s, v8::Object>, v8::Local<'s, v8::Value>> {
    debug_assert!(members_of(dict).is_some(), "`convert_dict_caught` names {dict}, which generated_ctor_dicts.rs does not hold: run scripts/sync.sh");
    let v = v8::Global::new(scope, v);
    let r = {
        v8::tc_scope!(let tc, scope);
        let v = v8::Local::new(tc, &v);
        match convert(tc, v, dict, prefix) {
            Ok(o) => Ok(v8::Global::new(tc, o)),
            Err(()) => {
                let e = tc.exception().map(|e| v8::Global::new(tc, e));
                tc.reset();
                Err(e)
            }
        }
    };
    match r {
        Ok(g) => Ok(v8::Local::new(scope, &g)),
        Err(Some(e)) => Err(v8::Local::new(scope, &e)),
        Err(None) => Err(v8::undefined(scope).into()),
    }
}

/// WebIDL's dictionary conversion of `v` to `dict`. `undefined` and `null` are the empty dictionary;
/// any other non-object is a TypeError.
fn convert<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    v: v8::Local<'s, v8::Value>,
    dict: &str,
    prefix: &str,
) -> Result<v8::Local<'s, v8::Object>, ()> {
    match convert_into(scope, v, dict, prefix, true, None)? {
        Some(o) => Ok(o),
        None => Ok(null_proto_object(scope)),
    }
}

/// [`convert_dict`] for a binding that only needs the conversion's effects -- its reads, in order, and its
/// throws -- and never reads the result (`createElement`'s `ElementCreationOptions`): the same conversion,
/// without materialising the converted object. `dict` must be a string literal, as for [`convert_dict`].
pub fn check_dict(scope: &mut v8::PinScope, v: v8::Local<'_, v8::Value>, dict: &str, prefix: &str) -> Result<(), ()> {
    debug_assert!(members_of(dict).is_some(), "`check_dict` names {dict}, which generated_ctor_dicts.rs does not hold: run scripts/sync.sh");
    let v = v8::Local::new(scope, v);
    convert_into(scope, v, dict, prefix, false, None).map(|_| ())
}

/// [`convert_dict`] for a binding that reads the converted members once, in Rust: the same conversion -- its reads,
/// in order, and its throws -- answering the members present (given or defaulted) as `(name, value)` pairs, without
/// the null-prototype object a binding would read them back from (`addEventListener`'s options: making it and
/// reading it was most of the call). `dict` must be a string literal, as for [`convert_dict`].
pub fn convert_dict_values<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    v: v8::Local<'_, v8::Value>,
    dict: &str,
    prefix: &str,
) -> Result<Vec<(&'static str, v8::Local<'s, v8::Value>)>, ()> {
    debug_assert!(members_of(dict).is_some(), "`convert_dict_values` names {dict}, which generated_ctor_dicts.rs does not hold: run scripts/sync.sh");
    let v = v8::Local::new(scope, v);
    let mut out = Vec::new();
    convert_into(scope, v, dict, prefix, false, Some(&mut out))?;
    Ok(out)
}

/// [`convert`]'s body; `keep` decides whether the converted object is made (`Some`) or only the conversion run, and
/// `values`, when given, collects each member present as it is converted.
fn convert_into<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    v: v8::Local<'s, v8::Value>,
    dict: &str,
    prefix: &str,
    keep: bool,
    mut values: Option<&mut Vec<(&'static str, v8::Local<'s, v8::Value>)>>,
) -> Result<Option<v8::Local<'s, v8::Object>>, ()> {
    // Blink's dictionary `Create` (bind_gen/dictionary.py): `undefined` and `null` are the empty dictionary only for a
    // dictionary with no required member (its own or inherited) -- for one with a required member they are "not of
    // type", like any other non-object (Chrome 150: `attachShadow(null)`, where WebIDL would read `mode` and say it
    // is missing).
    let members = members_of(dict);
    let has_required = members.is_some_and(|ms| ms.iter().any(|m| m.required));
    let src = if v.is_null_or_undefined() && !has_required {
        None
    } else if let Ok(o) = v8::Local::<v8::Object>::try_from(v) {
        Some(o)
    } else {
        throw_type_error(scope, &format!("{prefix}The provided value is not of type '{dict}'."));
        return Err(());
    };
    let out = keep.then(|| null_proto_object(scope));
    let Some(members) = members else { return Ok(out) };
    let mut window: Option<bool> = None;
    for m in members {
        // No source object (`undefined` / `null` for a dictionary with no required member): a member with
        // no default is absent from the result, and nothing is read -- skipped before the window test and
        // the context below, which have no effect a page can see.
        if src.is_none() && m.default.is_none() {
            continue;
        }
        // a member Blink exposes to windows alone is not read in a worker (`RequestInit.browsingTopics`)
        if super::generated_ctor_dicts::WINDOW_ONLY.contains(&(m.owner, m.name)) {
            let in_window = *window.get_or_insert_with(|| {
                let g = scope.get_current_context().global(scope);
                crate::binding::protos::implements(scope, g.into(), "Window")
            });
            if !in_window {
                continue;
            }
        }
        // The member's error context, written only if an error is: a `format!` per member was most of a
        // dictionary conversion that throws nothing.
        let at = At { prefix, name: m.name, owner: m.owner };
        let given = match src {
            Some(o) => match get_prop(scope, o, m.name) {
                Some(g) => g,
                None => return Err(()), // the getter threw
            },
            None => v8::undefined(scope).into(),
        };
        let value = if given.is_undefined() {
            if m.required {
                throw_type_error(scope, &format!("{at}Required member is undefined."));
                return Err(());
            }
            match m.default {
                Some(d) => Some(dflt_value(scope, d, m.ty, &at)?),
                None => None,
            }
        } else if m.nullable && given.is_null() {
            Some(given)
        } else if let (DictTy::Int { bits, signed }, Some(enforce)) = (m.ty, integer_attribute(m)) {
            Some(ranged_integer(scope, given, bits, signed, enforce, &at)?)
        } else {
            Some(member(scope, given, m.ty, &at)?)
        };
        if let (Some(v), Some(out)) = (value, out) {
            if let Some(k) = crate::binding::protos::key_str(scope, m.name) {
                out.create_data_property(scope, k.into(), v);
            }
        }
        if let (Some(v), Some(vs)) = (value, values.as_deref_mut()) {
            vs.push((m.name, v));
        }
        // `[DeprecateAs]`: Blink's binding asks `Has` after the member is read, present or not, to count its use
        if let (Some(o), true) = (src, super::generated_ctor_dicts::HAS_AFTER.contains(&(m.owner, m.name))) {
            let k = crate::binding::protos::key_val(scope, m.name);
            if o.has(scope, k).is_none() {
                return Err(());
            }
        }
    }
    Ok(out)
}

/// A fresh, extensible, null-prototype ordinary object: a converted dictionary. Made as `{}` with its prototype
/// set to null, not by `Object::with_prototype_and_properties(null, ..)`, which makes a dictionary-mode object:
/// V8 caches no transition out of a dictionary map, so each member added was a hash-table insert and every
/// later read of it a lookup, where this one's members are fast properties of a shared map.
fn null_proto_object<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Object> {
    let o = v8::Object::new(scope);
    let null: v8::Local<v8::Value> = v8::null(scope).into();
    o.set_prototype(scope, null);
    o
}

/// A dictionary member's error context, "<prefix>Failed to read the '<name>' property from '<owner>': ".
struct At<'a> {
    prefix: &'a str,
    name: &'static str,
    owner: &'static str,
}

impl std::fmt::Display for At<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}Failed to read the '{}' property from '{}': ", self.prefix, self.name, self.owner)
    }
}

/// An integer member's `[EnforceRange]` (`Some(true)`) or `[Clamp]` (`Some(false)`), as Blink 150's IDL declares it.
fn integer_attribute(m: &DictMember) -> Option<bool> {
    let key = (m.owner, m.name);
    if super::generated_ctor_dicts::ENFORCE_RANGE.contains(&key) {
        Some(true)
    } else if super::generated_ctor_dicts::CLAMP.contains(&key) {
        Some(false)
    } else {
        None
    }
}

/// The IDL integer type of `bits`/`signed`, as Blink's messages name it, and its range (64-bit types stop at
/// 2^53 - 1, the largest integer a JS number holds exactly).
fn integer_type(bits: u32, signed: bool) -> (&'static str, f64, f64) {
    const JS_MAX: f64 = 9007199254740991.0;
    match (bits, signed) {
        (8, true) => ("byte", -128.0, 127.0),
        (8, false) => ("octet", 0.0, 255.0),
        (16, true) => ("short", -32768.0, 32767.0),
        (16, false) => ("unsigned short", 0.0, 65535.0),
        (32, true) => ("long", -2147483648.0, 2147483647.0),
        (32, false) => ("unsigned long", 0.0, 4294967295.0),
        (_, true) => ("long long", -JS_MAX, JS_MAX),
        (_, false) => ("unsigned long long", 0.0, JS_MAX),
    }
}

/// Blink's `[EnforceRange]` (`enforce`) or `[Clamp]` integer conversion (bindings/core/v8/v8_binding_for_core.cc):
/// ToNumber; `[EnforceRange]` refuses a non-finite value ("Value is [infinite and ]not of type 'T'.") and a truncated
/// value outside T's range ("Value is outside the 'T' value range."); `[Clamp]` makes NaN 0 and puts the value into
/// the range -- an 8-, 16- or 32-bit one is then TRUNCATED (`ClampTo<T>(double)`: Chrome 150 makes
/// `streamErrorCode: 3.5` 3, where WebIDL rounds half to even), a 64-bit one rounded half to even first
/// (`std::nearbyint`). The plain conversion is modular ([`modular`]).
fn ranged_integer<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'s, v8::Value>, bits: u32, signed: bool, enforce: bool, at: &dyn std::fmt::Display) -> Result<v8::Local<'s, v8::Value>, ()> {
    let refused = if v.is_symbol() {
        Some("Cannot convert a Symbol value to a number")
    } else if v.is_big_int() {
        Some("Cannot convert a BigInt value to a number")
    } else {
        None
    };
    if let Some(why) = refused {
        throw_type_error(scope, &format!("{at}{why}"));
        return Err(());
    }
    let Some(x) = v.number_value(scope) else { return Err(()) };
    let (name, lo, hi) = integer_type(bits, signed);
    let n = if enforce {
        if !x.is_finite() {
            let inf = if x.is_infinite() { " infinite and" } else { "" };
            throw_type_error(scope, &format!("{at}Value is{inf} not of type '{name}'."));
            return Err(());
        }
        let t = x.trunc();
        if t < lo || t > hi {
            throw_type_error(scope, &format!("{at}Value is outside the '{name}' value range."));
            return Err(());
        }
        t + 0.0
    } else if x.is_nan() {
        0.0
    } else if bits == 64 {
        // round half to even (C's nearbyint), then into the range
        let r = x.round();
        let r = if (x - x.trunc()).abs() == 0.5 && r % 2.0 != 0.0 { r - x.signum() } else { r };
        r.clamp(lo, hi) + 0.0
    } else {
        // into the range, then truncated toward zero
        x.clamp(lo, hi).trunc() + 0.0
    };
    Ok(v8::Number::new(scope, n).into())
}

fn member<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    v: v8::Local<'s, v8::Value>,
    ty: DictTy,
    at: &dyn std::fmt::Display,
) -> Result<v8::Local<'s, v8::Value>, ()> {
    // A value V8 itself refuses to convert throws with the context in front ("Failed to read the 'name' property from
    // 'WorkerOptions': Cannot convert a Symbol value to a string"): Chrome's comes from V8's exception propagation
    // callback, which Blink fills from its dictionary stack, for a throw made in the binding itself. An exception the
    // page's own `toString`/`valueOf` throws passes through as thrown. (An OBJECT whose conversion V8 refuses -- a
    // `toString` answering an object -- is Chrome's prefixed throw too and is left to V8's bare message here.)
    let refused = match ty {
        DictTy::Str | DictTy::Enum(..) if v.is_symbol() => Some("Cannot convert a Symbol value to a string"),
        DictTy::Int { .. } | DictTy::Double { .. } if v.is_symbol() => Some("Cannot convert a Symbol value to a number"),
        DictTy::Int { .. } | DictTy::Double { .. } if v.is_big_int() => Some("Cannot convert a BigInt value to a number"),
        _ => None,
    };
    if let Some(why) = refused {
        throw_type_error(scope, &format!("{at}{why}"));
        return Err(());
    }
    Ok(match ty {
        DictTy::Any => v,
        DictTy::Object => {
            if !v.is_object() {
                throw_type_error(scope, &format!("{at}The provided value is not of type 'object'."));
                return Err(());
            }
            v
        }
        DictTy::Bool => v8::Boolean::new(scope, v.boolean_value(scope)).into(),
        DictTy::Int { bits, signed } => {
            let Some(n) = v.number_value(scope) else { return Err(()) };
            v8::Number::new(scope, modular(n, bits, signed)).into()
        }
        DictTy::Double { restricted, single } => {
            let Some(n) = v.number_value(scope) else { return Err(()) };
            if restricted && !n.is_finite() {
                let what = if single { "float" } else { "double" };
                throw_type_error(scope, &format!("{at}The provided {what} value is non-finite."));
                return Err(());
            }
            v8::Number::new(scope, if single { n as f32 as f64 } else { n }).into()
        }
        DictTy::Str => match v.to_string(scope) {
            Some(s) => s.into(),
            None => return Err(()),
        },
        DictTy::Enum(name, values) => {
            let Some(s) = v.to_string(scope) else { return Err(()) };
            let text = s.to_rust_string_lossy(scope);
            if !values.contains(&text.as_str()) {
                throw_type_error(scope, &format!("{at}The provided value '{text}' is not a valid enum value of type {name}."));
                return Err(());
            }
            s.into()
        }
        DictTy::Iface(names) => {
            if !names.iter().any(|n| is_a(scope, v, n)) {
                throw_type_error(scope, &format!("{at}Failed to convert value to '{}'.", names.join("' or '")));
                return Err(());
            }
            v
        }
        DictTy::Callback => {
            if !v.is_function() {
                throw_type_error(scope, &format!("{at}The given value is not a function."));
                return Err(());
            }
            v
        }
        DictTy::CallbackInterface => {
            if !v.is_object() {
                throw_type_error(scope, &format!("{at}The provided value is not of type 'object'."));
                return Err(());
            }
            v
        }
        DictTy::Seq { elem, frozen } => sequence(scope, v, elem, frozen, at)?,
        DictTy::Dict(name) => convert(scope, v, name, &at.to_string())?.into(),
    })
}

/// The sequence half of a `(sequence<T> or record<K, V>)` union, as WebIDL's union conversion decides it:
/// `v`'s `@@iterator` is read -- a `Get` a page can see -- and when it is callable the iterator's values
/// are the sequence (`Ok(Some(..))`); when it is `undefined` or `null` the value is the record (`Ok(None)`). A real
/// array is read by index, Blink's fast path. `Err` after throwing (`prefix` names the operation).
pub fn sequence_or_record<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'_, v8::Object>, prefix: &str) -> Result<Option<Vec<v8::Local<'s, v8::Value>>>, ()> {
    let v = v8::Local::new(scope, v);
    let sym = v8::Symbol::get_iterator(scope);
    let Some(method) = v.get(scope, sym.into()) else { return Err(()) };
    if method.is_null_or_undefined() {
        return Ok(None);
    }
    let Ok(method) = v8::Local::<v8::Function>::try_from(method) else {
        throw_type_error(scope, &format!("{prefix}The object must have a callable @@iterator property."));
        return Err(());
    };
    if let Ok(a) = v8::Local::<v8::Array>::try_from(v8::Local::<v8::Value>::from(v)) {
        let mut items = Vec::with_capacity(a.length() as usize);
        for i in 0..a.length() {
            let Some(x) = a.get_index(scope, i) else { return Err(()) };
            items.push(x);
        }
        return Ok(Some(items));
    }
    let Some(iter) = method.call(scope, v.into(), &[]) else { return Err(()) };
    let Ok(iter) = v8::Local::<v8::Object>::try_from(iter) else {
        throw_type_error(scope, &format!("{prefix}The provided value cannot be converted to a sequence."));
        return Err(());
    };
    let Some(next) = get_prop(scope, iter, "next") else { return Err(()) };
    let Ok(next) = v8::Local::<v8::Function>::try_from(next) else {
        throw_type_error(scope, &format!("{prefix}The provided value cannot be converted to a sequence."));
        return Err(());
    };
    let mut items = Vec::new();
    loop {
        let Some(r) = next.call(scope, iter.into(), &[]) else { return Err(()) };
        let Ok(r) = v8::Local::<v8::Object>::try_from(r) else {
            throw_type_error(scope, &format!("{prefix}The provided value cannot be converted to a sequence."));
            return Err(());
        };
        let Some(done) = get_prop(scope, r, "done") else { return Err(()) };
        if done.boolean_value(scope) {
            return Ok(Some(items));
        }
        let Some(x) = get_prop(scope, r, "value") else { return Err(()) };
        items.push(x);
    }
}

/// WebIDL "create a sequence from an iterable": `@@iterator` called once, `next()` until `done`, each
/// `value` converted to `elem`. A non-object is "cannot be converted to a sequence", an object with no
/// callable `@@iterator` Blink's own sentence.
fn sequence<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    v: v8::Local<'s, v8::Value>,
    elem: &DictTy,
    frozen: bool,
    at: &dyn std::fmt::Display,
) -> Result<v8::Local<'s, v8::Value>, ()> {
    let not_seq = |scope: &mut v8::PinScope| throw_type_error(scope, &format!("{at}The provided value cannot be converted to a sequence."));
    let Ok(obj) = v8::Local::<v8::Object>::try_from(v) else {
        not_seq(scope);
        return Err(());
    };
    let sym = v8::Symbol::get_iterator(scope);
    let Some(method) = obj.get(scope, sym.into()) else { return Err(()) };
    let Ok(method) = v8::Local::<v8::Function>::try_from(method) else {
        throw_type_error(scope, &format!("{at}The object must have a callable @@iterator property."));
        return Err(());
    };
    let Some(iter) = method.call(scope, obj.into(), &[]) else { return Err(()) };
    let Ok(iter) = v8::Local::<v8::Object>::try_from(iter) else {
        not_seq(scope);
        return Err(());
    };
    let Some(next) = get_prop(scope, iter, "next") else { return Err(()) };
    let Ok(next) = v8::Local::<v8::Function>::try_from(next) else {
        not_seq(scope);
        return Err(());
    };
    let mut items: Vec<v8::Local<v8::Value>> = Vec::new();
    loop {
        let Some(r) = next.call(scope, iter.into(), &[]) else { return Err(()) };
        let Ok(r) = v8::Local::<v8::Object>::try_from(r) else {
            not_seq(scope);
            return Err(());
        };
        let Some(done) = get_prop(scope, r, "done") else { return Err(()) };
        if done.boolean_value(scope) {
            break;
        }
        let Some(x) = get_prop(scope, r, "value") else { return Err(()) };
        items.push(member(scope, x, *elem, at)?);
    }
    let arr = v8::Array::new_with_elements(scope, &items);
    if frozen {
        let _ = arr.set_integrity_level(scope, v8::IntegrityLevel::Frozen);
    }
    Ok(arr.into())
}

/// An interface or buffer type's check: the buffer types by what the value IS, the rest by brand.
fn is_a(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, name: &str) -> bool {
    match name {
        "ArrayBuffer" => v.is_array_buffer(),
        "SharedArrayBuffer" => v.is_shared_array_buffer(),
        "ArrayBufferView" => v.is_array_buffer_view(),
        "BufferSource" | "AllowSharedBufferSource" => v.is_array_buffer() || v.is_array_buffer_view() || v.is_shared_array_buffer(),
        "DataView" => v.is_data_view(),
        "Int8Array" => v.is_int8_array(),
        "Uint8Array" => v.is_uint8_array(),
        "Uint8ClampedArray" => v.is_uint8_clamped_array(),
        "Int16Array" => v.is_int16_array(),
        "Uint16Array" => v.is_uint16_array(),
        "Int32Array" => v.is_int32_array(),
        "Uint32Array" => v.is_uint32_array(),
        "Float32Array" => v.is_float32_array(),
        "Float64Array" => v.is_float64_array(),
        "BigInt64Array" => v.is_big_int64_array(),
        "BigUint64Array" => v.is_big_uint64_array(),
        _ => crate::binding::protos::implements(scope, v, name),
    }
}

/// WebIDL's modular integer conversion (ToInt32 and friends): NaN and the infinities are 0, a
/// fraction truncates toward zero.
fn modular(n: f64, bits: u32, signed: bool) -> f64 {
    if !n.is_finite() {
        return 0.0;
    }
    let t = n.trunc();
    if bits >= 64 {
        return t;
    }
    let m = 2f64.powi(bits as i32);
    let mut r = t % m;
    if r < 0.0 {
        r += m;
    }
    if signed && r >= m / 2.0 {
        r -= m;
    }
    r
}

/// Whether Blink 150 declares `iface.attr` a `FrozenArray<T>`: `install::frozen_array`'s table, every interface's
/// (C2) -- the generated list this read before named only the shell constructors' interfaces, so a hand
/// constructor's FrozenArray was stored writable.
pub fn is_frozen_attr(iface: &str, attr: &str) -> bool {
    super::frozen_array::row(iface, attr).is_some()
}

/// A non-Event shell's own readonly attributes that NAME a constructor argument or a member of its
/// constructor dictionary read that value (gen_shells claims them with the branded slot getter):
/// Chrome's `new USBOutTransferResult("ok", 7).bytesWritten`, `new RTCError({errorDetail:
/// "data-channel-failure"}).errorDetail`, `new CountQueuingStrategy({highWaterMark: 3}).highWaterMark`.
/// A value the call left out (with no IDL default) reads its type's empty value -- `new
/// TextFormat().rangeStart` is 0 -- as the stub row answered before; a `FrozenArray` attribute holds a
/// frozen array.
pub fn ctor_values_to_slots(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, iface: &str, conv: &Converted) {
    use crate::install::MemberKind;
    let Some(def) = crate::install::generated_registry::REGISTRY.iter().find(|d| d.name == iface) else { return };
    let args = super::generated_ctor_dicts::CTOR_ARGS.iter().find(|(i, _)| *i == iface).map(|(_, a)| *a).unwrap_or(&[]);
    let members = conv.dict.and_then(|_| dict_arg(iface)).and_then(|(_, d)| members_of(d)).unwrap_or(&[]);
    for m in def.members.iter() {
        if m.static_member || !matches!(m.kind, MemberKind::Attr { readonly: true }) {
            continue;
        }
        if !args.iter().any(|a| a.name == m.name && !matches!(a.ty, DictTy::Dict(_))) && !members.iter().any(|d| d.name == m.name) {
            continue;
        }
        let k = crate::binding::protos::key_val(scope, m.name);
        let Ok(kn) = v8::Local::<v8::Name>::try_from(k) else { continue };
        // The value, and whether this conversion MADE it as a sequence (only such an array may be
        // frozen -- an `any` argument is the page's own object).
        let arg_seq = args.iter().any(|a| a.name == m.name && matches!(a.ty, DictTy::Seq { .. }));
        let member_seq = members.iter().any(|d| d.name == m.name && matches!(d.ty, DictTy::Seq { .. }));
        let mut v = None;
        let mut made_seq = false;
        for (src, seq) in [(Some(conv.named), arg_seq), (conv.dict.map(|(_, o)| o), member_seq)] {
            let Some(src) = src else { continue };
            if src.has_own_property(scope, kn).unwrap_or(false) {
                v = src.get(scope, k);
                made_seq = seq;
                break;
            }
        }
        let v = match v {
            Some(v) => v,
            None => {
                let d = crate::install::stub::stub_default(m.kind, m.ty, m.nullable);
                crate::install::stub::shape_value(scope, d)
            }
        };
        if made_seq && is_frozen_attr(iface, m.name) {
            if let Ok(a) = v8::Local::<v8::Object>::try_from(v) {
                let _ = a.set_integrity_level(scope, v8::IntegrityLevel::Frozen);
            }
        }
        crate::core::events::event::evt_slot_write(scope, this, m.name, v);
    }
}

#[cfg(test)]
mod tests {
    use super::At;

    #[test]
    fn at_display_matches_format() {
        for prefix in ["", "Failed to construct 'MouseEvent': ", "Échec de l'exécution de « x » : "] {
            let (name, owner) = ("clientX", "MouseEventInit");
            let old = format!("{prefix}Failed to read the '{}' property from '{}': ", name, owner);
            assert_eq!(At { prefix, name, owner }.to_string(), old);
            assert_eq!(format!("{}Required member is undefined.", At { prefix, name, owner }), format!("{old}Required member is undefined."));
        }
    }
}
