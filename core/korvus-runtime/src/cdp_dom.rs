//! What a DevTools client reads about objects and the DOM -- an object's kind, an element's
//! description and box, an element's handle in a world -- answered by the engine from its own arena and layout, never
//! through members a page can replace (a page must not be able to tell, or steer, what DevTools reads).
//!
//! Chrome 150's answers, recorded on the same form fixture: `~/bench-lean/cdp/probe/chrome150-page-shapes.jsonl`.
use deno_core::v8;

pub use webapi::page::NodeInfo;

/// What Chrome's `Runtime.RemoteObject` says about an object besides its id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectMeta {
    /// `type`: `"object"` or `"function"`.
    pub kind: &'static str,
    /// `subtype`: `node`, `array`, `error`, `promise`, `map`, `set`, `date`, `regexp`, or none.
    pub subtype: Option<&'static str>,
    /// `className`: the object's constructor, as V8 knows it (not a page-replaceable `constructor.name`).
    pub class_name: String,
    /// `description`: `input#q` for an element, `Array(3)`, `Map(2)`, else the class name.
    pub description: String,
}

impl crate::Engine {
    /// What stored object `id` is, as Chrome's `Runtime.RemoteObject` names it. Puppeteer and Playwright make an
    /// ELEMENT handle only of a `subtype: "node"` object -- without one neither can click, type or fill -- and this
    /// server named every object `{"type": "object", "className": "Object"}`.
    pub fn object_meta(&mut self, id: u32) -> Option<ObjectMeta> {
        let global = self.objects.get(&id)?.clone();
        let ctx = self.runtime.main_context();
        v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
        let scope: &mut v8::PinScope = scope;
        let v = v8::Local::new(scope, &global);
        if v.is_function() {
            return Some(ObjectMeta { kind: "function", subtype: None, class_name: "Function".into(), description: "Function".into() });
        }
        let obj = v8::Local::<v8::Object>::try_from(v).ok()?;
        let class_name = obj.get_constructor_name().to_rust_string_lossy(scope);
        let meta = |subtype: Option<&'static str>, description: String| ObjectMeta { kind: "object", subtype, class_name: class_name.clone(), description };
        if let Some(node) = webapi::page::node_id_of(scope, v) {
            let description = webapi::page::node_info(scope, node).map(|n| n.description).unwrap_or_else(|| class_name.clone());
            return Some(meta(Some("node"), description));
        }
        if let Ok(a) = v8::Local::<v8::Array>::try_from(v) {
            return Some(meta(Some("array"), format!("Array({})", a.length())));
        }
        if let Ok(m) = v8::Local::<v8::Map>::try_from(v) {
            return Some(meta(Some("map"), format!("Map({})", m.size())));
        }
        if let Ok(s) = v8::Local::<v8::Set>::try_from(v) {
            return Some(meta(Some("set"), format!("Set({})", s.size())));
        }
        let subtype = if v.is_native_error() {
            Some("error")
        } else if v.is_promise() {
            Some("promise")
        } else if v.is_date() {
            Some("date")
        } else if v.is_reg_exp() {
            Some("regexp")
        } else {
            None
        };
        Some(meta(subtype, class_name.clone()))
    }

    /// The arena element stored object `id` wraps, if it wraps one (CDP `objectId` → node).
    pub fn node_of_object(&mut self, id: u32) -> Option<u32> {
        let global = self.objects.get(&id)?.clone();
        let ctx = self.runtime.main_context();
        v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
        let scope: &mut v8::PinScope = scope;
        let v = v8::Local::new(scope, &global);
        webapi::page::node_id_of(scope, v)
    }

    /// A stored handle to element `node`, made in `world` (`None`: the page's own), and its id (CDP
    /// `DOM.resolveNode`). The wrapper is the node's one canonical object, so identity holds.
    pub fn object_for_node(&mut self, world: Option<usize>, node: u32) -> Option<u32> {
        let global = {
            let ctx = self.runtime.main_context();
            v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
            let scope: &mut v8::PinScope = scope;
            let ctx = match world.and_then(|w| webapi::devtools_realm_context(scope, w)) {
                Some(c) => c,
                None => scope.get_current_context(),
            };
            let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, ctx);
            webapi::page::node_info(scope, node)?;
            let obj = webapi::page::node_object(scope, node);
            v8::Global::new(scope, obj)
        };
        let id = self.next_object;
        self.next_object += 1;
        self.objects.insert(id, global);
        Some(id)
    }

    /// Element `node`, described from the arena (`DOM.describeNode`).
    pub fn node_info(&mut self, node: u32) -> Option<NodeInfo> {
        let ctx = self.runtime.main_context();
        v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
        let scope: &mut v8::PinScope = scope;
        webapi::page::node_info(scope, node)
    }

    /// Element `node`'s border box as one quad in the viewport, CSS px, clockwise from the top left
    /// (`DOM.getContentQuads`; Chrome 150 on the form fixture: `[[8,108,193,108,193,129,8,129]]`). `None` when it
    /// has no box -- Chrome's "Could not compute content quads."
    pub fn node_quad(&mut self, node: u32) -> Option<[f64; 8]> {
        let ctx = self.runtime.main_context();
        v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
        let scope: &mut v8::PinScope = scope;
        let (x, y, w, h) = webapi::page::node_page_rect(scope, node);
        (w > 0.0 || h > 0.0).then_some([x, y, x + w, y, x + w, y + h, x, y + h])
    }

    /// The frames of the page a DevTools client should know (`webapi::devtools_frames`), parents first.
    pub fn devtools_frames(&mut self) -> Vec<webapi::DevtoolsFrame> {
        self.with_scope(webapi::devtools_frames)
    }

    /// The realm index [`Engine::eval_remote_details_in`] and the rest name frame `id`'s own realm by.
    pub fn frame_realm_index(id: u32) -> usize {
        webapi::FRAME_REALM_BASE + id as usize
    }

    /// An isolated world `name` over frame `id`'s current document (`Page.createIsolatedWorld` with its id): its index.
    pub fn create_frame_world(&mut self, id: u32, name: &str) -> Option<usize> {
        self.with_scope(|scope| webapi::create_frame_world(scope, id, name))
    }

    /// The frame node `node`'s document belongs to: `Some(None)` the page's, `Some(Some(id))` a frame's.
    pub fn frame_of_node(&mut self, node: u32) -> Option<Option<u32>> {
        self.with_scope(|scope| webapi::frame_of_node(scope, node))
    }

    /// The frame frame element `el` holds.
    pub fn content_frame_of(&mut self, el: u32) -> Option<u32> {
        self.with_scope(|scope| webapi::content_frame_of(scope, el))
    }

    /// The popups this page's scripts opened that it shows itself (`webapi::devtools_popups`).
    pub fn devtools_popups(&mut self) -> Vec<webapi::DevtoolsPopup> {
        self.with_scope(webapi::devtools_popups)
    }

    /// `Target.closeTarget` on popup `id`'s target.
    pub fn close_popup(&mut self, id: u32) {
        self.with_scope(|scope| webapi::close_popup(scope, id));
    }

    /// `Page.navigate` on popup `id`'s target.
    pub fn navigate_popup(&mut self, id: u32, url: &str) {
        self.with_scope(|scope| webapi::navigate_popup(scope, id, url));
    }

    /// `Page.captureScreenshot` on popup `id`'s target.
    pub fn popup_shot(&mut self, id: u32, format: spec::ImageFormat, quality: Option<f64>) -> Option<Vec<u8>> {
        self.with_scope(|scope| {
            let root = webapi::popup_document_root(scope, id)?;
            webapi::render_popup_shot(scope, root, format, quality)
        })
    }

    /// `Input.dispatchMouseEvent` on popup `id`'s target: the mouse in the popup's window.
    #[allow(clippy::too_many_arguments)]
    pub fn popup_mouse_event(&mut self, id: u32, kind: webapi::input::MouseKind, x: f64, y: f64, button: i32, buttons: u16, click_count: i32, modifiers: webapi::input::Modifiers) -> usize {
        let ts = self.perf_now();
        self.with_scope(|scope| {
            let Some(root) = webapi::popup_document_root(scope, id) else { return 0 };
            let input = webapi::input::MouseInput { kind, x, y, button, buttons, click_count, modifiers, timestamp_ms: ts, coalesced: Vec::new() };
            webapi::input::mouse_input_at(scope, &input, Some(root))
        })
    }

    /// `DOM.scrollIntoViewIfNeeded`: scroll `node` (or `rect` of it, in its border box) into view, as Chrome does.
    pub fn scroll_into_view_if_needed(&mut self, node: u32, rect: Option<(f64, f64, f64, f64)>) {
        let ctx = self.runtime.main_context();
        v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
        let scope: &mut v8::PinScope = scope;
        webapi::page::scroll_node_into_view_if_needed(scope, node, rect);
    }

    /// The document's arena root and how many children it has (`DOM.getDocument`).
    pub fn document_node(&mut self) -> Option<(u32, usize)> {
        let ctx = self.runtime.main_context();
        v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
        let scope: &mut v8::PinScope = scope;
        webapi::page::with_host_dom(scope, |dom| (dom.root(), dom.children(dom.root()).len()))
    }

    /// Stored object `id`'s own properties and its prototype (`Runtime.getProperties` with `ownProperties`), read
    /// through V8's own property API: no getter runs, no page-replaceable `Object.*` is called. Every object value is
    /// stored and named by its new id. `None` for an id this engine never issued. A proxy lists nothing (its traps
    /// are not run).
    ///
    /// Puppeteer's `$$`, `$$eval` and `JSHandle.getProperties` read an array of element handles this way: Chrome 150
    /// listed each index with `writable`, `configurable`, `enumerable` and `isOwn`, then `length`, and the prototype
    /// as the internal property `[[Prototype]]` (`~/bench-lean/cdp/probe/chrome150-compat-traffic.log`).
    pub fn own_properties(&mut self, id: u32) -> Option<(Vec<PropertyRow>, Option<PropValue>)> {
        let global = self.objects.get(&id)?.clone();
        let ctx = self.runtime.main_context();
        v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
        let scope: &mut v8::PinScope = scope;
        let v = v8::Local::new(scope, &global);
        let Ok(obj) = v8::Local::<v8::Object>::try_from(v) else { return Some((Vec::new(), None)) };
        if v.is_proxy() {
            return Some((Vec::new(), None));
        }
        let args = v8::GetPropertyNamesArgs {
            mode: v8::KeyCollectionMode::OwnOnly,
            property_filter: v8::PropertyFilter::ALL_PROPERTIES,
            index_filter: v8::IndexFilter::IncludeIndices,
            key_conversion: v8::KeyConversionMode::ConvertToString,
        };
        let mut rows = Vec::new();
        if let Some(names) = obj.get_own_property_names(scope, args) {
            for i in 0..names.length() {
                let Some(key) = names.get_index(scope, i) else { continue };
                let Ok(name) = v8::Local::<v8::Name>::try_from(key) else { continue };
                let Some(desc) = obj.get_own_property_descriptor(scope, name) else { continue };
                let Ok(desc) = v8::Local::<v8::Object>::try_from(desc) else { continue };
                let symbol = key.is_symbol();
                let label = if symbol {
                    key.to_detail_string(scope).map(|s| s.to_rust_string_lossy(scope)).unwrap_or_default()
                } else {
                    key.to_rust_string_lossy(scope)
                };
                let writable = own_field(scope, desc, "writable").map(|x| x.is_true());
                let configurable = own_field(scope, desc, "configurable").is_some_and(|x| x.is_true());
                let enumerable = own_field(scope, desc, "enumerable").is_some_and(|x| x.is_true());
                let mut value = None;
                if let Some(x) = own_field(scope, desc, "value") {
                    value = Some(keep(scope, &mut self.next_object, &mut self.objects, x));
                }
                let mut get = None;
                if let Some(x) = own_field(scope, desc, "get").filter(|x| !x.is_undefined()) {
                    get = Some(keep(scope, &mut self.next_object, &mut self.objects, x));
                }
                let mut set = None;
                if let Some(x) = own_field(scope, desc, "set").filter(|x| !x.is_undefined()) {
                    set = Some(keep(scope, &mut self.next_object, &mut self.objects, x));
                }
                rows.push(PropertyRow { name: label, value, get, set, writable, configurable, enumerable, symbol });
            }
        }
        let mut proto = None;
        if let Some(p) = obj.get_prototype(scope).filter(|p| p.is_object()) {
            proto = Some(keep(scope, &mut self.next_object, &mut self.objects, p));
        }
        Some((rows, proto))
    }
}

/// One own property of a stored object, as `Runtime.getProperties` lists it ([`crate::Engine::own_properties`]).
#[derive(Debug, Clone)]
pub struct PropertyRow {
    /// The key: a string key as it is, a symbol key as V8 describes it (`Symbol(x)`).
    pub name: String,
    /// A data property's value.
    pub value: Option<PropValue>,
    /// An accessor property's getter and setter.
    pub get: Option<PropValue>,
    pub set: Option<PropValue>,
    /// A data property's `writable`; `None` for an accessor.
    pub writable: Option<bool>,
    pub configurable: bool,
    pub enumerable: bool,
    /// The key is a symbol.
    pub symbol: bool,
}

/// A property's value: a stored object (its id), or a primitive as its complete `Runtime.RemoteObject`.
#[derive(Debug, Clone)]
pub enum PropValue {
    Object(u32),
    Primitive(serde_json::Value),
}

/// A value as the protocol reports it: an object stored under a fresh id (as `remote_from_global` stores one), a
/// primitive as its RemoteObject.
fn keep<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    next: &mut u32,
    objects: &mut std::collections::HashMap<u32, v8::Global<v8::Value>>,
    x: v8::Local<'s, v8::Value>,
) -> PropValue {
    if x.is_object() {
        let id = *next;
        *next += 1;
        objects.insert(id, v8::Global::new(scope, x));
        PropValue::Object(id)
    } else {
        PropValue::Primitive(primitive_json(scope, x))
    }
}

/// A descriptor's own field `f` (a property descriptor object V8 just made: its fields are own data properties, so
/// reading one never reaches a prototype a page could have given a getter).
fn own_field<'s>(scope: &mut v8::PinScope<'s, '_>, desc: v8::Local<'s, v8::Object>, f: &str) -> Option<v8::Local<'s, v8::Value>> {
    let k = v8::String::new(scope, f)?;
    if !desc.has_own_property(scope, k.into()).unwrap_or(false) {
        return None;
    }
    desc.get(scope, k.into())
}

/// A primitive's `Runtime.RemoteObject`, in Chrome's shapes: a number's `description` its JS string and a number
/// JSON cannot hold (`NaN`, `Infinity`, `-0`) as `unserializableValue`; a bigint's as `1n`; `null` as an object of
/// subtype `null`.
fn primitive_json(scope: &mut v8::PinScope, x: v8::Local<v8::Value>) -> serde_json::Value {
    use serde_json::json;
    if x.is_undefined() {
        json!({ "type": "undefined" })
    } else if x.is_null() {
        json!({ "type": "object", "subtype": "null", "value": null })
    } else if x.is_boolean() {
        json!({ "type": "boolean", "value": x.is_true() })
    } else if x.is_string() {
        json!({ "type": "string", "value": x.to_rust_string_lossy(scope) })
    } else if x.is_number() {
        let n = x.number_value(scope).unwrap_or(f64::NAN);
        if n == 0.0 && n.is_sign_negative() {
            json!({ "type": "number", "unserializableValue": "-0", "description": "-0" })
        } else if !n.is_finite() {
            let d = x.to_rust_string_lossy(scope);
            json!({ "type": "number", "unserializableValue": d, "description": d })
        } else {
            let value = if n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 { json!(n as i64) } else { json!(n) };
            json!({ "type": "number", "value": value, "description": x.to_rust_string_lossy(scope) })
        }
    } else if x.is_big_int() {
        let d = format!("{}n", x.to_rust_string_lossy(scope));
        json!({ "type": "bigint", "unserializableValue": d, "description": d })
    } else if x.is_symbol() {
        let d = x.to_detail_string(scope).map(|s| s.to_rust_string_lossy(scope)).unwrap_or_default();
        json!({ "type": "symbol", "description": d })
    } else {
        json!({ "type": "undefined" })
    }
}
