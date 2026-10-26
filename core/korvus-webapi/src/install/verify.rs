//! Surface description, for the byte-identical cutover gate.
//!
//! Every one-table cutover must leave an interface's prototype surface unchanged: the same member
//! names, in the same order, with the same descriptor shape (accessor vs data, enumerable,
//! configurable). [`describe_own`] turns a prototype into a stable, comparable list so a test can
//! assert the table-installed surface equals the reference (the old JS-glue surface, or the bundle
//! selector). Member ORDER is part of the surface -- a fingerprint reads it -- so this preserves it.

use deno_core::v8;

/// One line per own property of `obj`, in V8's own-property order:
/// `"<name>:<accessor|data>:e=<0|1>:c=<0|1>[:w=<0|1>]"`. Accessor entries also note whether a setter
/// is present (`s=1`). The shape, not the value, is what a cutover must preserve.
pub fn describe_own<'s>(scope: &mut v8::PinScope<'s, '_>, obj: v8::Local<'s, v8::Object>) -> Vec<String> {
    let mut out = Vec::new();
    // `Object.getOwnPropertyNames` semantics: all own STRING keys, INCLUDING non-enumerable ones (the
    // default args are `ONLY_ENUMERABLE | SKIP_SYMBOLS`, which would drop a non-enum `constructor` and
    // make the surface look one member short). Own-only, skip symbols, keep non-enumerable.
    let args = v8::GetPropertyNamesArgsBuilder::new()
        .mode(v8::KeyCollectionMode::OwnOnly)
        .property_filter(v8::PropertyFilter::SKIP_SYMBOLS)
        .index_filter(v8::IndexFilter::IncludeIndices)
        .build();
    let Some(names) = obj.get_own_property_names(scope, args) else {
        return out;
    };
    for i in 0..names.length() {
        let Some(key) = names.get_index(scope, i) else { continue };
        let name = key.to_rust_string_lossy(scope);
        // A Symbol key stringifies to "Symbol(...)"; keep it, it is part of the surface too.
        let Ok(key_name) = v8::Local::<v8::Name>::try_from(key) else { continue };
        match obj.get_own_property_descriptor(scope, key_name) {
            Some(d) if d.is_object() => {
                let d = d.to_object(scope).unwrap();
                let has = |k: &str| -> bool {
                    v8::String::new(scope, k)
                        .and_then(|s| d.get(scope, s.into()))
                        .map(|v| !v.is_undefined())
                        .unwrap_or(false)
                };
                let flag = |k: &str| -> u8 {
                    v8::String::new(scope, k)
                        .and_then(|s| d.get(scope, s.into()))
                        .map(|v| if v.boolean_value(scope) { 1 } else { 0 })
                        .unwrap_or(0)
                };
                let is_accessor = has("get") || has("set");
                if is_accessor {
                    out.push(format!("{name}:accessor:e={}:c={}:s={}", flag("enumerable"), flag("configurable"), has("set") as u8));
                } else {
                    out.push(format!("{name}:data:e={}:c={}:w={}", flag("enumerable"), flag("configurable"), flag("writable")));
                }
            }
            _ => out.push(format!("{name}:?")),
        }
    }
    out
}
