//! `core/html/gpu_namespaces.rs` — WebGPU's five CONSTANT NAMESPACES on the window.
//!
//! `GPUBufferUsage`, `GPUColorWrite`, `GPUMapMode`, `GPUShaderStage` and `GPUTextureUsage` are the
//! only members of the WebGPU surface that are not interfaces: each is a WebIDL `namespace` holding
//! nothing but integer constants, and a page reads them to build the bitmask it passes to
//! `device.createBuffer` and friends. They were five of the 43 own globals this engine did not have.
//!
//! # Everything below is a reading, and the two browsers agree on all of it
//!
//! Brave 151.1.93.138 and Chrome 150.0.7871.46, headed, over CDP, 2026-09-19:
//!
//! ```text
//! window descriptor    {value, writable: true, enumerable: false, configurable: true}
//! [[Prototype]]        Object.prototype          Object.prototype.toString  [object GPUBufferUsage]
//! own string keys      the constants, in the order below, and nothing else -- no `constructor`
//! each constant        {value, writable: false, enumerable: true, configurable: false}
//! Symbol.toStringTag   present; the object is EXTENSIBLE, not sealed and not frozen
//! ```
//!
//! The values are the platform's, not a device's: they are the bit positions the WebGPU spec fixes,
//! and the two browsers answer the same integers. A bundle that does not record the name installs
//! nothing, which is [`crate::install::surface::publishes_global`]'s job and not a branch here.

use deno_core::v8;

use crate::binding::protos::str_val;
use crate::core::html::exposure::Exposure;

/// `(global name, [(constant, value)])` — ORDER IS THE DEVICE'S ENUMERATION ORDER, read off
/// `Object.getOwnPropertyNames` on both browsers, not the spec's declaration order.
///
/// All five carry `[SecureContext]` — WebGPU's whole surface does — and that is measured rather than
/// read off the spec: on `http://web-platform.test:<p>/` none of the five is an own property of
/// `window` on Chrome 150 or Brave 151, and on `http://127.0.0.1:<p>/` all five are.
/// The condition is carried through [`crate::core::html::exposure::gate`], which every name below
/// passes, so it is stated beside the name rather than applied to the group from outside.
const NAMESPACES: [(&str, &[(&str, u32)]); 5] = [
    (
        "GPUBufferUsage",
        &[
            ("MAP_READ", 1),
            ("MAP_WRITE", 2),
            ("COPY_SRC", 4),
            ("COPY_DST", 8),
            ("INDEX", 16),
            ("VERTEX", 32),
            ("UNIFORM", 64),
            ("STORAGE", 128),
            ("INDIRECT", 256),
            ("QUERY_RESOLVE", 512),
        ],
    ),
    ("GPUColorWrite", &[("RED", 1), ("GREEN", 2), ("BLUE", 4), ("ALPHA", 8), ("ALL", 15)]),
    ("GPUMapMode", &[("READ", 1), ("WRITE", 2)]),
    ("GPUShaderStage", &[("VERTEX", 1), ("FRAGMENT", 2), ("COMPUTE", 4)]),
    (
        "GPUTextureUsage",
        &[
            ("COPY_SRC", 1),
            ("COPY_DST", 2),
            ("TEXTURE_BINDING", 4),
            ("STORAGE_BINDING", 8),
            ("RENDER_ATTACHMENT", 16),
            ("TRANSIENT_ATTACHMENT", 32),
        ],
    ),
];

/// The five namespaces the ACTIVE capture records, each with the descriptor measured above.
pub(crate) fn install_window_gpu_namespaces<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    for (name, constants) in NAMESPACES {
        // `[SecureContext]`, measured: WebGPU's constant namespaces are gone on an insecure origin
        // on both browsers, along with the 60-odd `GPU*` INTERFACE objects beside them (that larger
        // set is the registry's and is FP5.7's recorded residual, not this file's).
        if !crate::core::html::exposure::gate(scope, crate::core::html::exposure::Publishes::Global, name, Exposure::SecureContext) {
            continue;
        }
        let ns = v8::Object::new(scope);
        for (k, v) in constants {
            let Some(key) = v8::String::new(scope, k) else { continue };
            let value = v8::Integer::new_from_unsigned(scope, *v);
            // A WebIDL constant: readonly and permanent, and ENUMERABLE -- `JSON.stringify(
            // GPUBufferUsage)` prints every one of them on both browsers.
            let mut d = v8::PropertyDescriptor::new_from_value_writable(value.into(), false);
            d.set_enumerable(true);
            d.set_configurable(false);
            ns.define_property(scope, key.into(), &d);
        }
        let tag = v8::Symbol::get_to_string_tag(scope);
        let tv = str_val(scope, name);
        let mut d = v8::PropertyDescriptor::new_from_value_writable(tv, false);
        d.set_enumerable(false);
        d.set_configurable(true);
        ns.define_property(scope, tag.into(), &d);

        let Some(k) = v8::String::new(scope, name) else { continue };
        // A namespace object's own descriptor on the global is an interface object's:
        // `{w:1, e:0, c:1}`. Configurable, so the registry walk's seat can move it to the capture's
        // position.
        let mut pd = v8::PropertyDescriptor::new_from_value_writable(ns.into(), true);
        pd.set_enumerable(false);
        pd.set_configurable(true);
        global.define_property(scope, k.into(), &pd);
    }
}
