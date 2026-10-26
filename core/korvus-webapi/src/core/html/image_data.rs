//! `core/html/image_data.rs` — `ImageData`.
//!
//! Was `globalThis.ImageData = class ImageData { … }` in `core/korvus-runtime/src/js/host.js`, whose
//! constructor wrote its four slot-backed members through the `KB.__evtSlotSet` bridge. It is an
//! `interface!` table with a native constructor now, and the five rows are behind the interface's
//! brand.
//!
//! MEASURED with `tests/fixtures/image-data-probe.js` on `google-chrome --headless=new`
//! 150.0.7871.46. The shape the JS class already had right is kept exactly: `data` is the ONE member a
//! real `ImageData` owns (`Object.getOwnPropertyNames(new ImageData(3,2))` is `["data"]`) while
//! `ImageData.prototype` separately carries a `data` getter of its own, and `width`/`height`/
//! `colorSpace`/`pixelFormat` are prototype accessors over internal slots.
//!
//! WHAT IT GOT WRONG, and what the native constructor answers now:
//!
//! * `ImageData("x")` said "Class constructor ImageData cannot be invoked without 'new'"; a browser
//!   says "Please use the 'new' operator, this DOM object constructor cannot be called as a function."
//! * `new ImageData(0, 1)` and `new ImageData(1, 0)` both answered "Invalid ImageData dimensions"
//!   under `IndexSizeError`. Chrome distinguishes them: "The source width is zero or not a number."
//!   and "The source height is zero or not a number.", and uses a THIRD message for a negative or
//!   oversized request ("The requested image size exceeds the supported range.").
//! * `new ImageData(new Uint8ClampedArray(5), 1)` was accepted and produced a 1×1 image over five
//!   bytes. Chrome throws `InvalidStateError`: "The input data length is not a multiple of 4."
//! * `new ImageData(new Uint8ClampedArray(8), 1, 3)` was accepted with a height of 3. Chrome throws
//!   `IndexSizeError`: "The input data length is not equal to (4 * width * height)."
//! * every row answered `undefined` for a foreign receiver; all ten receiver/row pairs throw
//!   `Illegal invocation` now.
//!
//! `new ImageData(2.9, 1.9)` is 2×1 in Chrome (the `[EnforceRange] unsigned long` truncation), which
//! the class already did with `Math.floor`.

use deno_core::v8;

use crate::binding::protos::{str_val, throw_dom};
use crate::core::events::event::{evt_slot_write, evt_slot_write_num};
use crate::install::webidl::{brand, init_dict, require_args, require_new};

/// `new ImageData(data, width[, height][, settings])` and `new ImageData(width, height[, settings])`
/// — `length` 2, both overloads two required arguments — as Blink's binding and `ImageData::ValidateAndCreate`
/// run them (stock Chrome 150 measured): the arguments are converted first -- the
/// settings' `colorSpace` then `pixelFormat`, each a TypeError when it is no IDL enum value -- then width, height,
/// the size, a colour space the build has not enabled, the data's array kind against the pixel format
/// ("Float16Array must use rgba-float16 pixelFormat."), and the data's length.
pub(crate) fn cb_image_data_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    use crate::core::html::pixel_format::{check_color_space_enabled, new_data_array, read_settings, PixelFormat};
    // ARITY BEFORE DIMENSIONS. `new ImageData()` is a TypeError, not the `IndexSizeError` a zero-size
    // image gets -- the class had that right and it stays right.
    if !require_new(scope, &args, "ImageData") || !require_args(scope, &args, "ImageData", 2) {
        return;
    }
    let a = args.get(0);
    // The first overload is the one whose `data` IS an `ImageDataArray` -- a `Uint8ClampedArray`, a
    // `Float16Array` or a `Float32Array`: the typed array's own kind, not `instanceof`. Chrome
    // 150, measured: another realm's array and a subclass's instance are data; `Object.create(...prototype)`
    // is not (it goes to the `(width, height)` overload and its ToNumber throws), nor is an `Int8Array`. And
    // `instanceof` runs JS, which a platform object rebuilt inside V8's ValueDeserializer (a worker's message)
    // must not do -- V8 aborts the process there ("Invoke in DisallowJavascriptExecutionScope").
    let data_kind = PixelFormat::of_array(a);
    let from_data = data_kind.is_some();
    // The argument count with trailing `undefined`s dropped, as Blink's overload dispatch counts it: a third
    // argument that is `undefined` is a missing height when it is the last one, and is converted (to 0) when the
    // settings follow it (Chrome 150: `new ImageData(u8, 2, undefined, {})` is "The source height is zero").
    let mut argc = args.length();
    while argc > 0 && args.get(argc - 1).is_undefined() {
        argc -= 1;
    }
    // Both overloads' dimensions are WebIDL `unsigned long` -- ToUint32, so NaN, +-Infinity, `undefined`
    // and 2^32 are 0 and -4294967295 is 1 -- and `{}` and `null` are height 0 (measured,
    // `tools/realweb/seed/imgdims`).
    let (width, height, settings_idx) = if from_data {
        // new ImageData(data, width[, height][, settings])
        let Some(w) = args.get(1).uint32_value(scope) else { return };
        let h = if argc >= 3 {
            let Some(h) = args.get(2).uint32_value(scope) else { return };
            Some(h)
        } else {
            None
        };
        (w, h, 3)
    } else {
        // new ImageData(width, height[, settings])
        let Some(w) = a.uint32_value(scope) else { return };
        let Some(h) = args.get(1).uint32_value(scope) else { return };
        (w, Some(h), 2)
    };
    let Ok(settings) = init_dict(scope, &args, settings_idx, "ImageData", "ImageDataSettings") else { return };
    let prefix = "Failed to construct 'ImageData': ";
    let Some((color_space, pixel_format)) = read_settings(scope, settings, prefix) else { return };
    let fmt = pixel_format.unwrap_or(PixelFormat::Unorm8);
    // `ValidateAndCreate`, in its order (measured: a zero width wins over a bad data length, a zero height over
    // empty data, the size over every data check, a disabled colour space over the data's kind).
    let fail = |scope: &mut v8::PinScope, msg: &str, name: &str| throw_dom(scope, &format!("{prefix}{msg}"), name);
    if width == 0 {
        fail(scope, "The source width is zero or not a number.", "IndexSizeError");
        return;
    }
    if width > i32::MAX as u32 {
        fail(scope, "The requested image size exceeds the supported range.", "IndexSizeError");
        return;
    }
    if let Some(h) = height {
        if h == 0 {
            fail(scope, "The source height is zero or not a number.", "IndexSizeError");
            return;
        }
        if h > i32::MAX as u32 {
            fail(scope, "The requested image size exceeds the supported range.", "IndexSizeError");
            return;
        }
    }
    if 4u64 * width as u64 * height.unwrap_or(0) as u64 > u32::MAX as u64 {
        fail(scope, "The requested image size exceeds the supported range.", "IndexSizeError");
        return;
    }
    let color_space = color_space.unwrap_or_else(|| "srgb".to_string());
    if !check_color_space_enabled(scope, &color_space, prefix) {
        return;
    }
    let height = match data_kind {
        Some(kind) => {
            if kind != fmt {
                fail(scope, &format!("{} must use {} pixelFormat.", kind.array_name(), kind.as_str()), "InvalidStateError");
                return;
            }
            let len = v8::Local::<v8::TypedArray>::try_from(a).map(|v| v.length()).unwrap_or(0) as u64;
            if len == 0 {
                fail(scope, "The input data has zero elements.", "InvalidStateError");
                return;
            }
            if len % 4 != 0 {
                fail(scope, "The input data length is not a multiple of 4.", "InvalidStateError");
                return;
            }
            let pixels = len / 4;
            if pixels % width as u64 != 0 {
                fail(scope, "The input data length is not a multiple of (4 * width).", "IndexSizeError");
                return;
            }
            let rows = pixels / width as u64;
            if height.is_some_and(|h| h as u64 != rows) {
                fail(scope, "The input data length is not equal to (4 * width * height).", "IndexSizeError");
                return;
            }
            rows as f64
        }
        None => height.unwrap_or(0) as f64,
    };
    let width = width as f64;
    let len = (width * height * 4.0) as usize;

    let this = args.this();
    evt_slot_write_num(scope, this, "width", width);
    evt_slot_write_num(scope, this, "height", height);
    let cs = str_val(scope, &color_space);
    evt_slot_write(scope, this, "colorSpace", cs);
    let pf = str_val(scope, fmt.as_str());
    evt_slot_write(scope, this, "pixelFormat", pf);
    let buf: v8::Local<v8::Value> = if from_data {
        a
    } else {
        match new_data_array(scope, fmt, None, len) {
            Some(u) => u,
            None => return,
        }
    };
    evt_slot_write(scope, this, "data", buf);
    // An `rgba-unorm8` ImageData keeps `data` as an own property of the instance AND as a getter on the prototype,
    // over one store (`Object.getOwnPropertyNames(new ImageData(3, 2))` is `["data"]`); a float one has no own
    // property -- its `data` is the prototype getter's (Chrome 150 and Brave 151, measured: a bot challenge
    // frame reads a float16 ImageData's `data` through the getter twenty times).
    if fmt == PixelFormat::Unorm8 {
        if let Some(k) = v8::String::new(scope, "data") {
            let mut pd = v8::PropertyDescriptor::new_from_value_writable(buf, false);
            pd.set_enumerable(true);
            pd.set_configurable(true);
            this.define_property(scope, k.into(), &pd);
        }
    }
    brand(scope, this, "ImageData");
    rv.set(this.into());
}

/// An `ImageData`'s `width` / `height` from its slot, NOT its page-visible getter (a page wrapping
/// `ImageData.prototype.width` must not see `putImageData` read it); `None` for anything else.
pub(crate) fn image_data_extent(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, key: &str) -> Option<f64> {
    crate::core::events::event::evt_slot_read(scope, o, key).and_then(|v| v.number_value(scope))
}

// Rows VERBATIM from the `ImageData` block of `install/generated_registry.rs` (identical to
// `generated_worker_registry.rs`'s), in the capture's order: width, height, colorSpace, data,
// pixelFormat. `[value, both]`: both captures record `newable: true` and `length: 2`.
//
// `evt_get` rather than `slot_get`: the branded read, so a foreign receiver throws
// `Illegal invocation` the way Chrome does. Only an interface whose constructor stamps a brand may
// name it, which this one now does.
webapi::interface! {
    "ImageData" [value, both] {
        ctor "ImageData"                       len 2 required 2 = stub,
        attr "width"       : Integer readonly                   = evt_get,
        attr "height"      : Integer readonly                   = evt_get,
        attr "colorSpace"  : DomString readonly                 = evt_get,
        attr "data"        : Object readonly                    = evt_get,
        attr "pixelFormat" : DomString readonly                 = evt_get
    }
}
