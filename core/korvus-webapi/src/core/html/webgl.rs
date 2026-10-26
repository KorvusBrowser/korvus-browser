//! `WebGLRenderingContext` / `WebGL2RenderingContext` — the object lifecycle and every draw call,
//! native, recording into a `spec::GlOp` command list that `korvus-render` executes at readback.
//!
//! ## What was here before
//!
//! Four methods: `getParameter`, `getExtension`, `getSupportedExtensions`, `getContextAttributes`,
//! all serving the recorded persona oracle, all correct. Everything else was a shell stub, so
//! `createBuffer()` returned `undefined`, `drawArrays` did nothing, `readPixels` wrote nothing, and
//! two different `clearColor` values produced byte-identical `toDataURL` strings. Those four stay
//! exactly as they were (the device identity is recorded replay and remains so); this file is the
//! other 130.
//!
//! ## The shape, and why it is the 2D one
//!
//! `canvas_2d.rs` records into a per-context display list held in `OpState` and keyed by a
//! `v8::Private` slot on the context object; `getImageData` and `toDataURL` both raster that list
//! through the active backend, which is what makes them agree. This is the same structure with
//! `GlOp` in place of `Canvas2DOp` — one store, one id, one list — because a second structure would
//! be a second place for the two readback paths to diverge.
//!
//! ## Why one callback instead of 138
//!
//! Every method is installed with `build_fn_data(cb_gl, <name>, <arity>, <name as data>)`: a real
//! native function, with the member's own name and the capture's own `length`, that reads which
//! member it is from its bound data. That is the same mechanism `canvas_2d`'s accessors already use.
//! A hundred and thirty near-identical `pub fn cb_gl_*` wrappers would carry no more information and
//! one more place to get an arity wrong.

use deno_core::v8;
use spec::{
    GlContextAttributes, GlObjectKind, GlOp, GlQuery, GlQueryResult, GlShaderKind,
    GlUniformValue, RenderParams, Rgba8,
};
use std::collections::{HashMap, HashSet};

use super::html_canvas_element::{priv_get, CANVAS_CTX_KIND_PRIV, CANVAS_CTX_PRIV};
// `shell_proto` survives at the three MINT sites whose interface is still a generated registry shell
// (`WebGLBuffer` and its ten siblings, `WebGLUniformLocation`): those calls read
// `globalThis[X].prototype` to WEAR it, which is not an install. The two mint sites whose interface
// this lane gave a table say so instead, through `set_table_proto`.
use crate::binding::protos::{build_fn_data, get_prop, int_val, require_args, shell_proto, str_val, throw_type_error};
use crate::install::live::getter_row;
use crate::install::MemberDef;

/// The four `interface!` tables — 2,090 rows of capture data, in their own file. See its module doc.
pub(crate) mod tables;
/// The extension objects `getExtension` hands out, and what a LOST context answers.
pub(crate) mod extensions;
/// `getParameter` as Blink's switch answers it, and the state it reads.
pub(crate) mod params;
/// WebGL 2's object state: samplers, queries, syncs, indexed bindings, buffer contents, immutable storage.
pub(crate) mod gl2;
pub(crate) mod gl2b;

/// The display-list id, on the context object.
pub(crate) const WEBGL_ID_PRIV: &str = "korvus_webgl_id";
/// A `WebGLBuffer`/`WebGLShader`/... object's numeric id.
const GL_OBJ_PRIV: &str = "korvus_webgl_obj";
/// A `WebGLUniformLocation`'s program and name.
const GL_LOC_PROG_PRIV: &str = "korvus_webgl_loc_prog";
const GL_LOC_NAME_PRIV: &str = "korvus_webgl_loc_name";

// ── store ─────────────────────────────────────────────────────────────────────────────────────

/// One WebGL context's recorded state.
pub struct WebGLState {
    pub ops: Vec<GlOp>,
    /// Which op list `ops` is: a number no list had before ([`new_list`]), taken again
    /// whenever the list starts over. Under one number `ops` only grows at its end, which is what lets
    /// the backend answer a query by going on from the ops it ran for the last one.
    pub(crate) list: u64,
    /// The attributes this context was created with — the backend rasterizes against these, and
    /// `getContextAttributes()` reads back the same struct, so the two cannot disagree.
    pub attrs: GlContextAttributes,
    pub v2: bool,
    next_obj: u32,
    /// How many non-zero `getError` answers the page has already been given. GL's error flags are
    /// read-and-clear and the backend replays the whole list every time, so the cursor lives here.
    errors_read: u32,
    /// Objects the ES 2.0 pipeline has no use for — queries, samplers, transform feedbacks, syncs.
    /// They are created, identified and deleted HERE rather than recorded as ops, because an op
    /// nothing executes is exactly the fiction this seam exists to prevent. Their observable
    /// behaviour (a real object, `is*` true until deleted) is real; their effect on pixels is
    /// nothing, which is also what they have in a pipeline with no occlusion queries.
    aux: HashSet<u32>,
    /// `WEBGL_lose_context.loseContext()` has run and no restore has completed.
    pub(crate) lost: bool,
    /// The errors a lost context reports, oldest first: `CONTEXT_LOST_WEBGL` once, then whatever
    /// `loseContext`/`restoreContext` synthesized while lost. The backend's replayed list is empty
    /// while lost, so these cannot ride it.
    pub(crate) lost_errors: std::collections::VecDeque<u32>,
    /// The `webglcontextlost` listener called `preventDefault()`, so `restoreContext()` may restore.
    pub(crate) restorable: bool,
    /// The extensions `getExtension` has ENABLED on this context, by canonical name. Their enums
    /// answer from then on; before, they are INVALID_ENUM (measured, `extensions::gate`).
    pub(crate) enabled: HashSet<String>,
    /// The state the extension setters write and their enums read back.
    pub(crate) ext: extensions::ExtState,
    /// Object ids that have been BOUND at least once -- GL's line between a name and an object, which
    /// `is*` reads.
    pub(crate) bound: HashSet<u32>,
    /// The extensions enabled, in the order they were (`COMPRESSED_TEXTURE_FORMATS` lists formats in
    /// the order their extensions added them).
    pub(crate) enabled_order: Vec<String>,
    /// What `getParameter` reads: the state every recorded op has set (see [`params`]).
    pub(crate) params: params::Params,
    /// WebGL 2's object state (`gl2`): samplers, queries, syncs, indexed bindings, buffer contents.
    pub(crate) gl2: gl2::Gl2State,
    /// Framebuffer attachments and formats, transform feedback, integer attributes (`gl2b`).
    pub(crate) draw: gl2b::DrawState,
    /// The errors this file raises itself, oldest first and each at most once -- Blink's
    /// `synthetic_errors_`, which `getError` hands out before asking GL. They were ops the backend
    /// replayed into a list it capped at 64, so a page that provoked 65 errors read `NO_ERROR` from
    /// then on, and every one of them made the op list longer.
    pub(crate) synthetic: Vec<u32>,
    /// The drawing-buffer size the list last recorded (`GlOp::ResizeDrawingBuffer`, [`record_size`]).
    pub(crate) buffer: (u32, u32),
    /// The backend that runs this context's lists: told when a list is let go, so the GPU context
    /// under it goes too ([`WebGLState::restart_list`], `Drop`).
    backend: Option<std::sync::Arc<dyn spec::RenderBackend>>,
    /// The backend's answers Chrome's client keeps ([`client_cached`]), by question, and the list and op count they
    /// were last checked against.
    client_cache: HashMap<String, GlQueryResult>,
    cache_mark: (u64, usize),
}

impl Default for WebGLState {
    fn default() -> Self {
        Self {
            ops: Vec::new(),
            list: new_list(),
            attrs: GlContextAttributes::default(),
            v2: false,
            next_obj: 0,
            errors_read: 0,
            aux: HashSet::new(),
            lost: false,
            lost_errors: std::collections::VecDeque::new(),
            restorable: false,
            enabled: HashSet::new(),
            ext: extensions::ExtState::default(),
            bound: HashSet::new(),
            enabled_order: Vec::new(),
            params: params::Params::default(),
            synthetic: Vec::new(),
            gl2: gl2::Gl2State::default(),
            draw: gl2b::DrawState::default(),
            buffer: (0, 0),
            backend: None,
            client_cache: HashMap::new(),
            cache_mark: (0, 0),
        }
    }
}

impl Drop for WebGLState {
    fn drop(&mut self) {
        if let Some(b) = &self.backend {
            b.release_gl(self.list);
        }
    }
}

/// A number no op list has had ([`WebGLState::list`]): process-wide, so a context made again under an
/// old id, or a list started over, is never taken for the one before.
pub(crate) fn new_list() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

#[derive(Default)]
pub struct WebGLStore {
    pub ctx: HashMap<u32, WebGLState>,
    next_id: u32,
}

/// Read — or allocate — the display-list id stashed on a WebGL context object.
pub(crate) fn webgl_id(
    scope: &mut v8::PinScope,
    obj: v8::Local<v8::Object>,
    op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>,
) -> u32 {
    let Some(pname) = v8::String::new(scope, WEBGL_ID_PRIV) else { return 0 };
    let key = v8::Private::for_api(scope, Some(pname));
    if let Some(id) = obj.get_private(scope, key).and_then(|v| v.uint32_value(scope)).filter(|&id| id != 0) {
        return id;
    }
    let id = {
        let mut st = op_state.borrow_mut();
        let s = st.borrow_mut::<WebGLStore>();
        s.next_id += 1;
        let id = s.next_id;
        s.ctx.entry(id).or_default();
        id
    };
    let idv = int_val(scope, id as i64);
    obj.set_private(scope, key, idv);
    id
}

/// `getContext('webgl'|'webgl2', opts)` calls this so the context starts with the attributes the
/// page asked for, seeded from the recorded device's own defaults.
pub(crate) fn init_context(
    scope: &mut v8::PinScope,
    ctx: v8::Local<v8::Object>,
    v2: bool,
    attrs: GlContextAttributes,
) {
    let (w, h) = (
        drawing_buffer_extent(scope, ctx, "width").unwrap_or(300) as i32,
        drawing_buffer_extent(scope, ctx, "height").unwrap_or(150) as i32,
    );
    let units = device_number(scope, MAX_COMBINED_TEXTURE_IMAGE_UNITS, v2).map(|n| n as u32);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let id = webgl_id(scope, ctx, &op_state);
    {
        let mut st = op_state.borrow_mut();
        let backend = st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::RenderHandle>()).map(|h| h.0.clone());
        let s = st.borrow_mut::<WebGLStore>().ctx.entry(id).or_default();
        s.v2 = v2;
        s.attrs = GlContextAttributes { webgl2: v2, ..attrs };
        s.backend = backend;
        s.params = params::Params::new(w, h);
        if let Some(n) = units {
            s.params.max_units = n;
        }
    }
    record_size(scope, ctx);
    // Blink's `InitializeNewContext` sets the viewport and the scissor box to the drawing buffer it
    // just made; they keep that size if the canvas is resized later, so the backend is told too.
    push(scope, ctx, GlOp::Viewport { x: 0, y: 0, width: w, height: h });
    push(scope, ctx, GlOp::Scissor { x: 0, y: 0, width: w, height: h });
}

const MAX_COMBINED_TEXTURE_IMAGE_UNITS: u32 = 0x8B4D;

/// `getParameter(pname)` on the context `ctx` -- see [`params`].
pub(crate) fn get_parameter(scope: &mut v8::PinScope, ctx: v8::Local<v8::Object>, pname: u32, v2: bool, rv: &mut v8::ReturnValue) {
    if v8::Local::<v8::Object>::try_from(oracle_value(scope)).is_err() {
        return;
    }
    // A name Chrome's GL client cannot answer itself is a round trip to the GPU (`params::round_trip`),
    // made when Blink would make it -- once the call is known valid, never for a lost context.
    let (round_trip, marks) = edit(scope, ctx, |s| ((!s.lost).then(|| params::round_trip(s, pname)).flatten(), (s.synthetic.len(), s.ops.len())));
    let gpu_get = |scope: &mut v8::PinScope| {
        if let Some(gl_name) = round_trip {
            let _ = ask(scope, ctx, GlQuery::Get { pname: gl_name });
        }
    };
    // A lost context, an extension enum whose extension is not enabled, and the extension state enums
    // (measured -- see `extensions`).
    if extensions::get_parameter(scope, ctx, pname, rv) {
        // an extension's name Blink reads from GL, when the call raised nothing (its extension is enabled)
        if edit(scope, ctx, |s| (s.synthetic.len(), s.ops.len())) == marks {
            gpu_get(scope);
        }
        return;
    }
    // `TRANSFORM_FEEDBACK_ACTIVE` / `_PAUSED`: the bound transform feedback's state (`gl2b`).
    if v2 {
        if let Some(b) = edit(scope, ctx, |s| gl2b::tf_param(s, pname)) {
            gpu_get(scope);
            rv.set_bool(b);
            return;
        }
    }
    let max_draw_buffers = device_number(scope, 0x8824, true).unwrap_or(1.0) as u32;
    let answer = edit(scope, ctx, |s| params::classify(s, pname, max_draw_buffers));
    use params::Answer as A;
    if !matches!(answer, A::Invalid) {
        gpu_get(scope);
    }
    match answer {
        A::Invalid => {
            push(scope, ctx, GlOp::RaiseError { code: INVALID_ENUM });
            rv.set_null();
        }
        A::Bool(b) => rv.set_bool(b),
        A::Uint(u) => rv.set_uint32(u),
        A::Int(i) => rv.set_int32(i),
        A::Float(f) => rv.set_double(f as f64),
        A::Number(n) => rv.set_double(n),
        A::Floats(v) => match new_f32_array(scope, &v) {
            Some(a) => rv.set(a),
            None => rv.set_null(),
        },
        A::Ints(v) => match new_i32_array(scope, &v) {
            Some(a) => rv.set(a),
            None => rv.set_null(),
        },
        A::Uints(v) => {
            let store = v8::ArrayBuffer::new(scope, v.len() * 4);
            match v8::Uint32Array::new(scope, store, 0, v.len()) {
                Some(t) => {
                    for (i, x) in v.iter().enumerate() {
                        let n = v8::Number::new(scope, *x as f64);
                        t.set_index(scope, i as u32, n.into());
                    }
                    rv.set(t.into());
                }
                None => rv.set_null(),
            }
        }
        // `sequence<boolean>`: a plain Array (Blink's GetBooleanArrayParameter).
        A::Bools(v) => {
            let a = v8::Array::new(scope, v.len() as i32);
            for (i, b) in v.iter().enumerate() {
                let x = v8::Boolean::new(scope, *b);
                a.set_index(scope, i as u32, x.into());
            }
            rv.set(a.into());
        }
        A::Object(id) => match object_of(scope, ctx, id) {
            Some(o) => rv.set(o),
            None => rv.set_null(),
        },
        A::Device => device_parameter(scope, pname, v2, if v2 { &["paramsByNumV2", "paramsByNum"] } else { &["paramsByNum"] }, rv),
        A::DeviceV2 => device_parameter(scope, pname, v2, &["paramsByNumV2"], rv),
        A::DeviceOr(d) => match device_value(scope, pname, if v2 { &["paramsByNumV2", "paramsByNum"] } else { &["paramsByNum"] }) {
            Some(v) => rv.set(v),
            None => rv.set_double(d),
        },
    }
}

/// A device parameter from the persona, in the type Blink returns it: the four strings by name,
/// `ALIASED_*_RANGE` a `Float32Array`, `MAX_VIEWPORT_DIMS` an `Int32Array`, every other one a number.
fn device_parameter(scope: &mut v8::PinScope, pname: u32, v2: bool, tables: &[&str], rv: &mut v8::ReturnValue) {
    let special = match pname {
        7936 => Some("vendor"),
        7937 => Some("renderer"),
        7938 => Some(if v2 { "versionV2" } else { "versionV1" }),
        35724 => Some(if v2 { "shadingV2" } else { "shadingV1" }),
        37445 => Some("unmaskedVendor"),
        37446 => Some("unmaskedRenderer"),
        _ => None,
    };
    if let Some(k) = special {
        let webgl = v8::Local::<v8::Object>::try_from(oracle_value(scope)).ok();
        match webgl.and_then(|w| get_prop(scope, w, k)) {
            Some(v) => rv.set(v),
            None => rv.set_null(),
        }
        return;
    }
    let Some(v) = device_value(scope, pname, tables) else {
        rv.set_null();
        return;
    };
    if let Ok(arr) = v8::Local::<v8::Array>::try_from(v) {
        let mut nums = Vec::new();
        for i in 0..arr.length() {
            nums.push(arr.get_index(scope, i).and_then(|e| e.number_value(scope)).unwrap_or(0.0));
        }
        let out = match pname {
            33901 | 33902 => new_f32_array(scope, &nums.iter().map(|n| *n as f32).collect::<Vec<_>>()),
            _ => new_i32_array(scope, &nums.iter().map(|n| *n as i32).collect::<Vec<_>>()),
        };
        match out {
            Some(a) => rv.set(a),
            None => rv.set_null(),
        }
        return;
    }
    rv.set(v);
}

/// `deviceOracle.webgl.internalformatSamples["<format>"]`, when the capture recorded it.
fn recorded_samples(scope: &mut v8::PinScope, format: u32) -> Option<Vec<i32>> {
    let webgl = v8::Local::<v8::Object>::try_from(oracle_value(scope)).ok()?;
    let table = get_prop(scope, webgl, "internalformatSamples").and_then(|t| v8::Local::<v8::Object>::try_from(t).ok())?;
    let row = get_prop(scope, table, &format.to_string()).and_then(|r| v8::Local::<v8::Array>::try_from(r).ok())?;
    let mut out = Vec::new();
    for i in 0..row.length() {
        out.push(row.get_index(scope, i)?.int32_value(scope)?);
    }
    Some(out)
}

/// `deviceOracle.webgl.precision["VERTEX_SHADER.LOW_FLOAT"]` and its eleven siblings.
fn recorded_precision(scope: &mut v8::PinScope, key: &str) -> Option<(i32, i32, i32)> {
    let webgl = v8::Local::<v8::Object>::try_from(oracle_value(scope)).ok()?;
    let table = get_prop(scope, webgl, "precision").and_then(|t| v8::Local::<v8::Object>::try_from(t).ok())?;
    let row = get_prop(scope, table, key).and_then(|r| v8::Local::<v8::Array>::try_from(r).ok())?;
    let mut n = [0i32; 3];
    for (i, slot) in n.iter_mut().enumerate() {
        *slot = row.get_index(scope, i as u32)?.int32_value(scope)?;
    }
    Some((n[0], n[1], n[2]))
}

/// The recorded device's numeric `getParameter` value for `pname` (the WebGL 2 table first for a
/// WebGL 2 context), when the bundle carries one.
pub(crate) fn device_number(scope: &mut v8::PinScope, pname: u32, v2: bool) -> Option<f64> {
    let v = device_value(scope, pname, if v2 { &["paramsByNumV2", "paramsByNum"] } else { &["paramsByNum"] })?;
    v.number_value(scope).filter(|_| v.is_number())
}

/// The recorded device's value for `pname` from the first of `tables` that has it.
pub(crate) fn device_value<'s>(scope: &mut v8::PinScope<'s, '_>, pname: u32, tables: &[&str]) -> Option<v8::Local<'s, v8::Value>> {
    let webgl = v8::Local::<v8::Object>::try_from(oracle_value(scope)).ok()?;
    for table in tables {
        let Some(t) = crate::binding::protos::get_prop(scope, webgl, table).and_then(|t| v8::Local::<v8::Object>::try_from(t).ok()) else {
            continue;
        };
        if let Some(v) = crate::binding::protos::get_prop(scope, t, &pname.to_string()).filter(|v| !v.is_undefined()) {
            return Some(v);
        }
    }
    None
}

/// A read-only accessor over a value stashed on a v8 Private of the instance.
///
/// `WebGLActiveInfo.name` and `WebGLShaderPrecisionFormat.precision` are prototype ACCESSORS in
/// every browser, and the instances have no own properties at all. Setting own data properties would
/// have read back the right values and shown three extra names to
/// `Object.getOwnPropertyNames(info)` -- a shape difference a page can check in one expression, and
/// exactly the kind of convenience this engine does not take.
fn cb_gl_slot_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let slot = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let Some(pname) = v8::String::new(scope, &format!("korvus_gl_{slot}")) else { return };
    let key = v8::Private::for_api(scope, Some(pname));
    match args.this().get_private(scope, key) {
        Some(v) if !v.is_undefined() => rv.set(v),
        _ => rv.set_undefined(),
    }
}

fn set_slot(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, slot: &str, v: v8::Local<v8::Value>) {
    if let Some(pname) = v8::String::new(scope, &format!("korvus_gl_{slot}")) {
        let key = v8::Private::for_api(scope, Some(pname));
        obj.set_private(scope, key, v);
    }
}

/// The attributes this context was created with — what `getContextAttributes()` reports and what
/// the backend rasterizes against. One struct, so the two cannot describe different buffers.
pub(crate) fn context_attributes(
    scope: &mut v8::PinScope,
    ctx: v8::Local<v8::Object>,
) -> GlContextAttributes {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let id = webgl_id(scope, ctx, &op_state);
    let st = op_state.borrow();
    st.borrow::<WebGLStore>().ctx.get(&id).map(|s| s.attrs.clone()).unwrap_or_default()
}

/// Record one op. A macro rather than a plain call because almost every op is built from arguments
/// that themselves need the scope, and `push(scope, this, GlOp::X { uint(scope, ..) })` borrows it
/// twice; this evaluates the op first.
macro_rules! rec {
    ($scope:expr, $this:expr, $op:expr $(,)?) => {{
        let op = $op;
        push($scope, $this, op);
    }};
}

/// Record `op` -- or, when the call is invalid, only the error it raises: a setter Chrome refuses
/// leaves the state as it was, so the backend must not see it and `getParameter` must not either
/// ([`params::Params::check`]).
fn push(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, op: GlOp) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let id = webgl_id(scope, this, &op_state);
    let mut st = op_state.borrow_mut();
    let s = st.borrow_mut::<WebGLStore>().ctx.entry(id).or_default();
    let code = match op {
        GlOp::RaiseError { code } => Some(code),
        _ => s.params.check(&op, s.v2, &s.enabled),
    };
    if let Some(code) = code {
        s.synthesize(code);
        return;
    }
    s.params.apply(&op);
    s.ops.push(op);
}

impl WebGLState {
    /// The list starts over under a number no list had ([`new_list`]); the backend lets the old one's context go.
    pub(crate) fn restart_list(&mut self) {
        if let Some(b) = &self.backend {
            b.release_gl(self.list);
        }
        self.list = new_list();
    }

    /// Blink's `SynthesizeGLError`: queue `code` unless it is already queued.
    pub(crate) fn synthesize(&mut self, code: u32) {
        if !self.synthetic.contains(&code) {
            self.synthetic.push(code);
        }
    }
}

/// An error raised by a call the store answers itself.
fn raise(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, code: Option<u32>) {
    if let Some(code) = code {
        push(scope, this, GlOp::RaiseError { code });
    }
}

/// Each context's objects by id, so a binding reads back the SAME object the page bound
/// (`getParameter(ARRAY_BUFFER_BINDING) === buf`) and `getAttachedShaders` the shaders it attached.
const GL_OBJECTS_PRIV: &str = "korvus_webgl_objects";

fn remember(scope: &mut v8::PinScope, ctx: v8::Local<v8::Object>, id: u32, obj: v8::Local<v8::Value>) {
    let map = match priv_get(scope, ctx, GL_OBJECTS_PRIV).and_then(|m| v8::Local::<v8::Map>::try_from(m).ok()) {
        Some(m) => m,
        None => {
            let m = v8::Map::new(scope);
            super::html_canvas_element::priv_set(scope, ctx, GL_OBJECTS_PRIV, m.into());
            m
        }
    };
    let k = int_val(scope, id as i64);
    map.set(scope, k, obj);
}

fn object_of<'s>(scope: &mut v8::PinScope<'s, '_>, ctx: v8::Local<v8::Object>, id: u32) -> Option<v8::Local<'s, v8::Value>> {
    if id == 0 {
        return None;
    }
    let map = priv_get(scope, ctx, GL_OBJECTS_PRIV).and_then(|m| v8::Local::<v8::Map>::try_from(m).ok())?;
    let k = int_val(scope, id as i64);
    map.get(scope, k).filter(|v| v.is_object())
}

/// A new WebGL object of interface `proto`, remembered under `id` on the context.
fn new_object<'s>(scope: &mut v8::PinScope<'s, '_>, ctx: v8::Local<v8::Object>, proto: &str, id: u32) -> Option<v8::Local<'s, v8::Value>> {
    let o = new_gl_object(scope, proto, id)?;
    remember(scope, ctx, id, o);
    Some(o)
}

fn edit<R>(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, f: impl FnOnce(&mut WebGLState) -> R) -> R {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let id = webgl_id(scope, this, &op_state);
    let mut st = op_state.borrow_mut();
    f(st.borrow_mut::<WebGLStore>().ctx.entry(id).or_default())
}

/// Record that `id` has been bound (a no-op for GL's null object).
fn bound(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, id: u32) {
    if id != 0 {
        edit(scope, this, |s| {
            s.bound.insert(id);
        });
    }
}

/// The next object id for this context. Ids are per-context and never zero (`0` is GL's null
/// object), so a handle from one canvas cannot be mistaken for one from another.
fn mint(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> u32 {
    edit(scope, this, |s| {
        s.next_obj += 1;
        s.next_obj
    })
}

/// The context's drawing-buffer size — its canvas's backing store, exactly as `toDataURL` reads it.
fn buffer_size(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> (u32, u32) {
    let canvas = priv_get(scope, this, super::canvas_2d::CANVAS_BACKREF_PRIV).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    let dim = |scope: &mut v8::PinScope, key: &str, dflt: f64| -> u32 {
        canvas
            .and_then(|c| super::html_canvas_element::canvas_extent(scope, c, key))
            .filter(|n| n.is_finite() && *n > 0.0)
            .unwrap_or(dflt) as u32
    };
    (dim(scope, "width", 300.0), dim(scope, "height", 150.0))
}

/// Record the drawing buffer's size when it is not the one the list last recorded: a new context's
/// buffer, and a canvas's whose `width` / `height` changed ([`canvas_resized`]) -- Blink's `Reshape`.
/// The list then says which size each draw went into, and the backend reallocates (and clears) the
/// buffer where the page did, not at whatever size the canvas has when a question is asked.
pub(crate) fn record_size(scope: &mut v8::PinScope, ctx: v8::Local<v8::Object>) {
    if extensions::is_lost(scope, ctx) {
        return;
    }
    let size = buffer_size(scope, ctx);
    let changed = edit(scope, ctx, |s| std::mem::replace(&mut s.buffer, size) != size);
    if changed {
        push(scope, ctx, GlOp::ResizeDrawingBuffer { width: size.0, height: size.1 });
    }
}

/// A canvas's `width` or `height` was written (`html_canvas_element::canvas_attr_changed`, and an
/// `OffscreenCanvas`'s setters): its WebGL context's drawing buffer follows, when its size changed.
///
/// Before this the list had no idea of a resize: every question was answered at the canvas's size of
/// the moment, so a page that drew at 27x57 and then at 10x10 had its 27x57 frame replayed into a
/// 10x10 buffer -- and a backend that keeps a machine per list had to throw it away at every new size
/// (a bot-detection script draws its proof of work at a new size each frame).
pub(crate) fn canvas_resized(scope: &mut v8::PinScope, canvas: v8::Local<v8::Object>) {
    let kind = match priv_get(scope, canvas, CANVAS_CTX_KIND_PRIV) {
        Some(k) => k.to_rust_string_lossy(scope),
        None => return,
    };
    if kind != "webgl" && kind != "webgl2" {
        return;
    }
    let Some(ctx) = priv_get(scope, canvas, CANVAS_CTX_PRIV).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
    record_size(scope, ctx);
}

/// Ask the backend a synchronous question about this context's recorded list -- on a GPU, a round trip to it, as
/// Chrome's are to its GPU process; a question Chrome's own client answers from what it already knows
/// ([`client_cached`]) is asked once and then answered here.
fn ask(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, q: GlQuery) -> GlQueryResult {
    let (w, h) = buffer_size(scope, this);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let id = webgl_id(scope, this, &op_state);
    let key = client_cached(&q).then(|| format!("{q:?}"));
    if let Some(key) = &key {
        let mut st = op_state.borrow_mut();
        if let Some(s) = st.borrow_mut::<WebGLStore>().ctx.get_mut(&id) {
            // an op since the answers were kept that could change one of them: they are asked again
            let (list, seen) = s.cache_mark;
            if list != s.list || s.ops.get(seen..).is_none_or(|new| new.iter().any(changes_cached)) {
                s.client_cache.clear();
            }
            s.cache_mark = (s.list, s.ops.len());
            if let Some(r) = s.client_cache.get(key) {
                return r.clone();
            }
        }
    }
    let answer = {
        let st = op_state.borrow();
        let Some(backend) = st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::RenderHandle>()).map(|h| h.0.clone()) else {
            return GlQueryResult::None;
        };
        // The list is read in place, not copied: the backend is Rust and calls back into nothing here.
        match st.borrow::<WebGLStore>().ctx.get(&id) {
            Some(s) => backend.query_gl(&s.ops, &q, w, h, &s.attrs, &RenderParams::default(), Some(s.list)),
            None => backend.query_gl(&[], &q, w, h, &GlContextAttributes::default(), &RenderParams::default(), None),
        }
    };
    if let Some(key) = key {
        if let Some(s) = op_state.borrow_mut().borrow_mut::<WebGLStore>().ctx.get_mut(&id) {
            s.client_cache.insert(key, answer.clone());
        }
    }
    answer
}

/// The questions Chrome answers without its GPU process (measured on desktop Chrome 150: 5,000-28,000 calls per ms,
/// where a round trip allows ~60): Blink's own program, shader and vertex-array objects, and the GL client's
/// `ProgramInfoManager`, which one round trip fills after a link (`GLES2Implementation::GetProgramivHelper` &
/// co.). `VALIDATE_STATUS` is the program parameter it asks GL for every time.
fn client_cached(q: &GlQuery) -> bool {
    match q {
        GlQuery::ProgramParameter { pname, .. } => *pname != 0x8B83,
        GlQuery::ShaderSource { .. }
        | GlQuery::AttachedShaders { .. }
        | GlQuery::AttribLocation { .. }
        | GlQuery::UniformLocation { .. }
        | GlQuery::ActiveAttrib { .. }
        | GlQuery::ActiveUniform { .. }
        | GlQuery::VertexAttrib { .. }
        | GlQuery::VertexAttribOffset { .. } => true,
        _ => false,
    }
}

/// An op after which a kept answer ([`client_cached`]) may not be the backend's any more: a program's link and its
/// shaders, a shader's source, the vertex arrays' state, and the objects those name.
fn changes_cached(op: &GlOp) -> bool {
    matches!(
        op,
        GlOp::ShaderSource { .. }
            | GlOp::AttachShader { .. }
            | GlOp::DetachShader { .. }
            | GlOp::DeleteShader { .. }
            | GlOp::DeleteProgram { .. }
            | GlOp::LinkProgram { .. }
            | GlOp::DeleteBuffer { .. }
            | GlOp::BindVertexArray { .. }
            | GlOp::DeleteVertexArray { .. }
            | GlOp::EnableVertexAttribArray { .. }
            | GlOp::DisableVertexAttribArray { .. }
            | GlOp::VertexAttribPointer { .. }
            | GlOp::VertexAttrib { .. }
            | GlOp::VertexAttribDivisor { .. }
    )
}

/// The drawing buffer, for `canvas.toDataURL()` / `toBlob()` on a WebGL canvas.
///
/// This is the WebGL half of `encode_canvas`'s 2D path, and it exists so a canvas whose context is
/// `webgl` serializes what the page drew instead of a buffer nothing ever wrote to.
pub(crate) fn raster_canvas(
    scope: &mut v8::PinScope,
    canvas: v8::Local<v8::Object>,
    width: u32,
    height: u32,
) -> Option<Rgba8> {
    let kind = priv_get(scope, canvas, CANVAS_CTX_KIND_PRIV)?.to_rust_string_lossy(scope);
    if kind != "webgl" && kind != "webgl2" {
        return None;
    }
    let ctx = v8::Local::<v8::Object>::try_from(priv_get(scope, canvas, CANVAS_CTX_PRIV)?).ok()?;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let id = webgl_id(scope, ctx, &op_state);
    let (ops, attrs, list) = {
        let st = op_state.borrow();
        match st.borrow::<WebGLStore>().ctx.get(&id) {
            Some(s) => (s.ops.clone(), s.attrs.clone(), Some(s.list)),
            None => (Vec::new(), GlContextAttributes::default(), None),
        }
    };
    let backend = {
        let st = op_state.borrow();
        st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::RenderHandle>()).map(|h| h.0.clone())
    };
    Some(match backend {
        Some(b) => b.raster_gl(&ops, width, height, &attrs, &RenderParams::default(), list),
        None => Rgba8::transparent(width, height),
    })
}

// ── argument reading ──────────────────────────────────────────────────────────────────────────

fn num(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, i: i32) -> f64 {
    args.get(i).number_value(scope).filter(|n| n.is_finite()).unwrap_or(0.0)
}
fn int(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, i: i32) -> i32 {
    num(scope, args, i) as i32
}
fn uint(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, i: i32) -> u32 {
    let n = num(scope, args, i);
    if n < 0.0 {
        n as i64 as u32
    } else {
        n as u32
    }
}
fn flt(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, i: i32) -> f32 {
    num(scope, args, i) as f32
}
fn boolean(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, i: i32) -> bool {
    args.get(i).boolean_value(scope)
}
fn text(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, i: i32) -> String {
    args.get(i).to_rust_string_lossy(scope)
}

/// A sequence of numbers, from a typed array OR a plain `Array` — pages pass both, and WebGL's IDL
/// accepts both (`Float32List` is `Float32Array or sequence<GLfloat>`).
fn f32_seq(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Vec<f32> {
    let Ok(obj) = v8::Local::<v8::Object>::try_from(v) else { return Vec::new() };
    let len = get_prop(scope, obj, "length").and_then(|l| l.uint32_value(scope)).unwrap_or(0);
    (0..len)
        .map(|i| obj.get_index(scope, i).and_then(|x| x.number_value(scope)).unwrap_or(0.0) as f32)
        .collect()
}

fn i32_seq(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Vec<i32> {
    let Ok(obj) = v8::Local::<v8::Object>::try_from(v) else { return Vec::new() };
    let len = get_prop(scope, obj, "length").and_then(|l| l.uint32_value(scope)).unwrap_or(0);
    (0..len)
        .map(|i| obj.get_index(scope, i).and_then(|x| x.number_value(scope)).unwrap_or(0.0) as i32)
        .collect()
}

/// The RAW BYTES behind an `ArrayBuffer` or any `ArrayBufferView` — what `bufferData` uploads. The
/// element type matters here (a `Float32Array` of 3 is 12 bytes), which is why this is not `f32_seq`.
fn raw_bytes(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<Vec<u8>> {
    if let Ok(view) = v8::Local::<v8::ArrayBufferView>::try_from(v) {
        let mut out = vec![0u8; view.byte_length()];
        view.copy_contents(&mut out);
        return Some(out);
    }
    if let Ok(ab) = v8::Local::<v8::ArrayBuffer>::try_from(v) {
        let len = ab.byte_length();
        let view = v8::Uint8Array::new(scope, ab, 0, len)?;
        let mut out = vec![0u8; len];
        view.copy_contents(&mut out);
        return Some(out);
    }
    None
}

/// A WebGL object handle's id, or `0` for `null`/`undefined` (GL's null object).
fn obj_id(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> u32 {
    let Ok(o) = v8::Local::<v8::Object>::try_from(v) else { return 0 };
    let Some(pname) = v8::String::new(scope, GL_OBJ_PRIV) else { return 0 };
    let key = v8::Private::for_api(scope, Some(pname));
    o.get_private(scope, key).and_then(|x| x.uint32_value(scope)).unwrap_or(0)
}

/// A `WebGLUniformLocation`'s (program, name).
fn loc_of(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<(u32, String)> {
    let o = v8::Local::<v8::Object>::try_from(v).ok()?;
    let pp = v8::String::new(scope, GL_LOC_PROG_PRIV)?;
    let pk = v8::Private::for_api(scope, Some(pp));
    let prog = o.get_private(scope, pk).and_then(|x| x.uint32_value(scope)).filter(|&p| p != 0)?;
    let np = v8::String::new(scope, GL_LOC_NAME_PRIV)?;
    let nk = v8::Private::for_api(scope, Some(np));
    let name = o.get_private(scope, nk).filter(|x| x.is_string())?.to_rust_string_lossy(scope);
    Some((prog, name))
}

/// A fresh `WebGL*` instance: the right prototype, and its id on a private slot so no JS reflection
/// path (`getOwnPropertyNames`, `Reflect.ownKeys`, a proxy) can see it. A real browser's WebGL
/// objects have no own properties at all, and neither do these.
fn new_gl_object<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    proto_name: &str,
    id: u32,
) -> Option<v8::Local<'s, v8::Value>> {
    let proto = shell_proto(scope, proto_name)?;
    let obj = crate::binding::protos::mint_on(scope, proto.into());
    let pname = v8::String::new(scope, GL_OBJ_PRIV)?;
    let key = v8::Private::for_api(scope, Some(pname));
    let idv = int_val(scope, id as i64);
    obj.set_private(scope, key, idv);
    Some(obj.into())
}

fn null_val<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Value> {
    v8::null(scope).into()
}

// ── the WebGL enums the recorder itself has to understand ─────────────────────────────────────

const VERTEX_SHADER: u32 = 0x8B31;
const FRAGMENT_SHADER: u32 = 0x8B30;
const UNPACK_FLIP_Y_WEBGL: u32 = 0x9240;
const FRONT_AND_BACK: u32 = 0x0408;
const RGBA: u32 = 0x1908;
const RGB: u32 = 0x1907;
const ALPHA: u32 = 0x1906;
const LUMINANCE: u32 = 0x1909;
const LUMINANCE_ALPHA: u32 = 0x190A;
const UNSIGNED_BYTE: u32 = 0x1401;
const FLOAT: u32 = 0x1406;
const HIGH_FLOAT: u32 = 0x8DF2;
const MEDIUM_FLOAT: u32 = 0x8DF1;
const LOW_FLOAT: u32 = 0x8DF0;
const INVALID_ENUM: u32 = 0x0500;
const INVALID_OPERATION: u32 = 0x0502;

/// Decode a `texImage2D` pixel upload into RGBA8. The formats a WebGL 1 context accepts for a
/// client-side upload are few, and every one of them lands on the same four channels.
fn decode_texels(bytes: &[u8], width: u32, height: u32, format: u32, ty: u32) -> Option<Rgba8> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 {
        return None;
    }
    let mut data = vec![0u8; w * h * 4];
    let comps = match format {
        RGBA | LUMINANCE_ALPHA if format == RGBA => 4,
        RGBA => 4,
        RGB => 3,
        LUMINANCE_ALPHA => 2,
        ALPHA | LUMINANCE => 1,
        _ => 4,
    };
    // Only the 8-bit and float paths, converted to 8-bit; the packed 16-bit types (5_6_5, 4_4_4_4,
    // 5_5_5_1) are read as their unpacked equivalents, which is what a page that uploads them then
    // samples them back would see to within the packing's own precision.
    let elem = match ty {
        FLOAT => 4usize,
        _ => 1,
    };
    for i in 0..(w * h) {
        let mut c = [0u8, 0, 0, 255];
        for k in 0..comps {
            let at = (i * comps + k) * elem;
            if at + elem > bytes.len() {
                break;
            }
            c[k] = match ty {
                FLOAT => {
                    let f = f32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
                    (f.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
                }
                _ => bytes[at],
            };
        }
        let px = match format {
            RGB => [c[0], c[1], c[2], 255],
            LUMINANCE => [c[0], c[0], c[0], 255],
            ALPHA => [0, 0, 0, c[0]],
            LUMINANCE_ALPHA => [c[0], c[0], c[0], c[1]],
            _ => c,
        };
        data[i * 4..i * 4 + 4].copy_from_slice(&px);
    }
    Some(Rgba8 { width, height, data })
}

/// `UNPACK_FLIP_Y_WEBGL` — applied here rather than in the backend, because it is a WebGL-level
/// unpack rule about the SOURCE, not something the GL pipeline knows anything about.
fn flip_rows(img: Rgba8) -> Rgba8 {
    let (w, h) = (img.width as usize, img.height as usize);
    let mut data = vec![0u8; w * h * 4];
    for y in 0..h {
        let src = (h - 1 - y) * w * 4;
        data[y * w * 4..(y + 1) * w * 4].copy_from_slice(&img.data[src..src + w * 4]);
    }
    Rgba8 { data, ..img }
}

// ── the dispatcher ────────────────────────────────────────────────────────────────────────────

/// Every WebGL method, keyed by the name bound as this function's `data` at install time.
pub(crate) fn cb_gl(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let member = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let this = args.this();
    gl_call(scope, this, &member, &args, &mut rv);
}

/// The body of every WebGL method, for the context `this` -- shared by the context's own rows and by
/// the extension objects whose methods are the same calls under another name (`drawArraysInstancedANGLE`
/// IS `drawArraysInstanced` on the context the extension belongs to).
pub(crate) fn gl_call(
    scope: &mut v8::PinScope,
    this: v8::Local<v8::Object>,
    member: &str,
    args: &v8::FunctionCallbackArguments,
    rv: &mut v8::ReturnValue,
) {
    let member = member.to_string();
    let iface = if edit(scope, this, |s| s.v2) { "WebGL2RenderingContext" } else { "WebGLRenderingContext" };
    let need = |scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, n: usize| -> bool {
        require_args(scope, args, n, iface, &member)
    };
    // A LOST CONTEXT answers every call the way Chrome's does, before any of the bodies
    // below records anything. See `extensions::lost_call` for the measured table.
    if edit(scope, this, |s| s.lost) {
        extensions::lost_call(scope, this, &member, args, rv);
        return;
    }
    // An EXTENSION ENUM before its extension is enabled is INVALID_ENUM (measured, `extensions::gate`).
    if matches!(member.as_str(), "enable" | "disable" | "isEnabled" | "hint")
        && { let cap = uint(scope, args, 0); extensions::gated(scope, this, cap) }
    {
        rec!(scope, this, GlOp::RaiseError { code: INVALID_ENUM });
        if member == "isEnabled" {
            rv.set_bool(false);
        }
        return;
    }

    // The validation and state Blink keeps beside the command buffer (`gl2b`), then WebGL 2's object
    // state (`gl2`); a `false` falls through to the bodies below.
    if gl2b::call(scope, this, &member, args, rv, iface) {
        return;
    }
    // WebGL 2's object state answers its own calls (`gl2`); a `false` falls through to the bodies below.
    if iface == "WebGL2RenderingContext" && gl2::call(scope, this, &member, args, rv, iface) {
        return;
    }

    match member.as_str() {
        // ── object lifecycle ──────────────────────────────────────────────────────────────────
        "createBuffer" | "createFramebuffer" | "createProgram" | "createRenderbuffer"
        | "createTexture" | "createVertexArray" => {
            let id = mint(scope, this);
            let (op, proto) = match member.as_str() {
                "createBuffer" => (GlOp::CreateBuffer { id }, "WebGLBuffer"),
                "createFramebuffer" => (GlOp::CreateFramebuffer { id }, "WebGLFramebuffer"),
                "createProgram" => (GlOp::CreateProgram { id }, "WebGLProgram"),
                "createRenderbuffer" => (GlOp::CreateRenderbuffer { id }, "WebGLRenderbuffer"),
                "createTexture" => (GlOp::CreateTexture { id }, "WebGLTexture"),
                _ => (GlOp::CreateVertexArray { id }, "WebGLVertexArrayObject"),
            };
            rec!(scope, this, op);
            match new_object(scope, this, proto, id) {
                Some(o) => rv.set(o),
                None => rv.set(null_val(scope)),
            }
        }
        "createShader" => {
            if !need(scope, args, 1) {
                return;
            }
            let kind = match uint(scope, args, 0) {
                VERTEX_SHADER => GlShaderKind::Vertex,
                FRAGMENT_SHADER => GlShaderKind::Fragment,
                // An unrecognised type is `null` PLUS an INVALID_ENUM the page can read back with
                // `getError()`. The error goes through `RaiseError` rather than a counter here, so
                // there is one error queue and `getError`'s read-and-clear cursor keeps working.
                _ => {
                    rec!(scope, this, GlOp::RaiseError { code: INVALID_ENUM });
                    rv.set(null_val(scope));
                    return;
                }
            };
            let id = mint(scope, this);
            rec!(scope, this, GlOp::CreateShader { id, kind });
            match new_object(scope, this, "WebGLShader", id) {
                Some(o) => rv.set(o),
                None => rv.set(null_val(scope)),
            }
        }
        // Objects the ES 2.0 pipeline does not consume: real identities, tracked here. See `aux`.
        "createQuery" | "createSampler" | "createTransformFeedback" => {
            let id = mint(scope, this);
            edit(scope, this, |s| {
                s.aux.insert(id);
            });
            let proto = match member.as_str() {
                "createQuery" => "WebGLQuery",
                "createSampler" => "WebGLSampler",
                _ => "WebGLTransformFeedback",
            };
            match new_object(scope, this, proto, id) {
                Some(o) => rv.set(o),
                None => rv.set(null_val(scope)),
            }
        }
        "fenceSync" => {
            let id = mint(scope, this);
            edit(scope, this, |s| {
                s.aux.insert(id);
            });
            match new_object(scope, this, "WebGLSync", id) {
                Some(o) => rv.set(o),
                None => rv.set(null_val(scope)),
            }
        }
        "deleteBuffer" | "deleteFramebuffer" | "deleteProgram" | "deleteRenderbuffer"
        | "deleteShader" | "deleteTexture" | "deleteVertexArray" => {
            if !need(scope, args, 1) {
                return;
            }
            let id = obj_id(scope, args.get(0));
            if id == 0 {
                return;
            }
            let op = match member.as_str() {
                "deleteBuffer" => GlOp::DeleteBuffer { id },
                "deleteFramebuffer" => GlOp::DeleteFramebuffer { id },
                "deleteProgram" => GlOp::DeleteProgram { id },
                "deleteRenderbuffer" => GlOp::DeleteRenderbuffer { id },
                "deleteShader" => GlOp::DeleteShader { id },
                "deleteTexture" => GlOp::DeleteTexture { id },
                _ => GlOp::DeleteVertexArray { id },
            };
            if matches!(member.as_str(), "deleteFramebuffer" | "deleteRenderbuffer" | "deleteTexture") {
                edit(scope, this, |s| gl2b::note_deleted(s, id));
            }
            rec!(scope, this, op);
        }
        "deleteQuery" | "deleteSampler" | "deleteTransformFeedback" | "deleteSync" => {
            if !need(scope, args, 1) {
                return;
            }
            let id = obj_id(scope, args.get(0));
            edit(scope, this, |s| {
                s.aux.remove(&id);
            });
        }
        "isBuffer" | "isFramebuffer" | "isProgram" | "isRenderbuffer" | "isShader" | "isTexture"
        | "isVertexArray" => {
            if !need(scope, args, 1) {
                return;
            }
            let id = obj_id(scope, args.get(0));
            let kind = match member.as_str() {
                "isBuffer" => GlObjectKind::Buffer,
                "isFramebuffer" => GlObjectKind::Framebuffer,
                "isProgram" => GlObjectKind::Program,
                "isRenderbuffer" => GlObjectKind::Renderbuffer,
                "isShader" => GlObjectKind::Shader,
                "isTexture" => GlObjectKind::Texture,
                _ => GlObjectKind::VertexArray,
            };
            // Buffers, textures, framebuffers, renderbuffers and vertex arrays are OBJECTS only once they
            // have been bound -- `isBuffer(createBuffer())` is false in Chrome 150 until `bindBuffer`
            // (measured). Programs and shaders are objects from creation.
            let needs_bind = !matches!(kind, GlObjectKind::Program | GlObjectKind::Shader);
            let r = id != 0
                && (!needs_bind || edit(scope, this, |s| s.bound.contains(&id)))
                && matches!(ask(scope, this, GlQuery::IsObject { kind, id }), GlQueryResult::Bool(true));
            rv.set_bool(r);
        }
        "isQuery" | "isSampler" | "isTransformFeedback" | "isSync" => {
            if !need(scope, args, 1) {
                return;
            }
            let id = obj_id(scope, args.get(0));
            // A query and a transform feedback are objects only once begun / bound; a sampler and a sync
            // are at creation (measured on Chrome 150).
            let needs_bind = matches!(member.as_str(), "isQuery" | "isTransformFeedback");
            let r = id != 0 && edit(scope, this, |s| s.aux.contains(&id) && (!needs_bind || s.bound.contains(&id)));
            rv.set_bool(r);
        }

        // ── shaders and programs ──────────────────────────────────────────────────────────────
        "shaderSource" => {
            if !need(scope, args, 2) {
                return;
            }
            let shader = obj_id(scope, args.get(0));
            let source = text(scope, args, 1);
            rec!(scope, this, GlOp::ShaderSource { shader, source });
        }
        "compileShader" => {
            if !need(scope, args, 1) {
                return;
            }
            let shader = obj_id(scope, args.get(0));
            rec!(scope, this, GlOp::CompileShader { shader });
        }
        "attachShader" | "detachShader" => {
            if !need(scope, args, 2) {
                return;
            }
            let program = obj_id(scope, args.get(0));
            let shader = obj_id(scope, args.get(1));
            rec!(
                scope,
                this,
                if member == "attachShader" {
                    GlOp::AttachShader { program, shader }
                } else {
                    GlOp::DetachShader { program, shader }
                },
            );
        }
        "bindAttribLocation" => {
            if !need(scope, args, 3) {
                return;
            }
            let program = obj_id(scope, args.get(0));
            let index = uint(scope, args, 1);
            let name = text(scope, args, 2);
            rec!(scope, this, GlOp::BindAttribLocation { program, index, name });
        }
        "linkProgram" | "validateProgram" | "useProgram" => {
            if !need(scope, args, 1) {
                return;
            }
            let program = obj_id(scope, args.get(0));
            // `useProgram` of a program that did not link is INVALID_OPERATION and changes nothing
            // (Blink: "program not valid"), before GL sees it.
            if member == "useProgram"
                && program != 0
                && !edit(scope, this, |s| s.params.check(&GlOp::UseProgram { program }, s.v2, &s.enabled).is_some())
                && !matches!(ask(scope, this, GlQuery::ProgramParameter { program, pname: 0x8B82 }), GlQueryResult::Bool(true))
            {
                rec!(scope, this, GlOp::RaiseError { code: params::INVALID_OPERATION });
                return;
            }
            rec!(
                scope,
                this,
                match member.as_str() {
                    "linkProgram" => GlOp::LinkProgram { program },
                    "validateProgram" => GlOp::ValidateProgram { program },
                    _ => GlOp::UseProgram { program },
                },
            );
        }
        "getShaderParameter" => {
            if !need(scope, args, 2) {
                return;
            }
            let shader = obj_id(scope, args.get(0));
            let pname = uint(scope, args, 1);
            let r = ask(scope, this, GlQuery::ShaderParameter { shader, pname });
            set_result(scope, rv, r);
        }
        "getProgramParameter" => {
            if !need(scope, args, 2) {
                return;
            }
            let program = obj_id(scope, args.get(0));
            let pname = uint(scope, args, 1);
            let r = ask(scope, this, GlQuery::ProgramParameter { program, pname });
            set_result(scope, rv, r);
        }
        "getShaderInfoLog" | "getShaderSource" => {
            if !need(scope, args, 1) {
                return;
            }
            let shader = obj_id(scope, args.get(0));
            let q = if member == "getShaderInfoLog" {
                GlQuery::ShaderInfoLog { shader }
            } else {
                GlQuery::ShaderSource { shader }
            };
            let r = ask(scope, this, q);
            set_result(scope, rv, r);
        }
        "getProgramInfoLog" => {
            if !need(scope, args, 1) {
                return;
            }
            let program = obj_id(scope, args.get(0));
            let r = ask(scope, this, GlQuery::ProgramInfoLog { program });
            set_result(scope, rv, r);
        }
        "getAttachedShaders" => {
            if !need(scope, args, 1) {
                return;
            }
            let program = obj_id(scope, args.get(0));
            match ask(scope, this, GlQuery::AttachedShaders { program }) {
                GlQueryResult::Ints(ids) => {
                    let arr = v8::Array::new(scope, ids.len() as i32);
                    for (i, id) in ids.iter().enumerate() {
                        if let Some(o) = object_of(scope, this, *id as u32).or_else(|| new_gl_object(scope, "WebGLShader", *id as u32)) {
                            arr.set_index(scope, i as u32, o);
                        }
                    }
                    rv.set(arr.into());
                }
                _ => rv.set(null_val(scope)),
            }
        }
        "getAttribLocation" => {
            if !need(scope, args, 2) {
                return;
            }
            let program = obj_id(scope, args.get(0));
            let name = text(scope, args, 1);
            match ask(scope, this, GlQuery::AttribLocation { program, name }) {
                GlQueryResult::Int(i) => rv.set_int32(i),
                _ => rv.set_int32(-1),
            }
        }
        "getUniformLocation" => {
            if !need(scope, args, 2) {
                return;
            }
            let program = obj_id(scope, args.get(0));
            let name = text(scope, args, 1);
            let found = !matches!(
                ask(scope, this, GlQuery::UniformLocation { program, name: name.clone() }),
                GlQueryResult::None
            );
            if !found {
                rv.set(null_val(scope));
                return;
            }
            // The location is an OPAQUE handle carrying (program, name) on private slots — the
            // shape a real `WebGLUniformLocation` has (no own properties at all).
            let Some(proto) = shell_proto(scope, "WebGLUniformLocation") else {
                rv.set(null_val(scope));
                return;
            };
            let o = crate::binding::protos::mint_on(scope, proto.into());
            if let Some(pn) = v8::String::new(scope, GL_LOC_PROG_PRIV) {
                let k = v8::Private::for_api(scope, Some(pn));
                let v = int_val(scope, program as i64);
                o.set_private(scope, k, v);
            }
            if let Some(pn) = v8::String::new(scope, GL_LOC_NAME_PRIV) {
                let k = v8::Private::for_api(scope, Some(pn));
                let v = str_val(scope, &name);
                o.set_private(scope, k, v);
            }
            rv.set(o.into());
        }
        "getActiveAttrib" | "getActiveUniform" => {
            if !need(scope, args, 2) {
                return;
            }
            let program = obj_id(scope, args.get(0));
            let index = uint(scope, args, 1);
            let q = if member == "getActiveAttrib" {
                GlQuery::ActiveAttrib { program, index }
            } else {
                GlQuery::ActiveUniform { program, index }
            };
            match ask(scope, this, q) {
                GlQueryResult::ActiveInfo { name, size, ty } => {
                    let o = crate::install::live::mint_table(scope, "WebGLActiveInfo");
                    let nv = str_val(scope, &name);
                    set_slot(scope, o, "name", nv);
                    let sv = int_val(scope, size as i64);
                    set_slot(scope, o, "size", sv);
                    let tv = v8::Number::new(scope, ty as f64).into();
                    set_slot(scope, o, "type", tv);
                    rv.set(o.into());
                }
                _ => rv.set(null_val(scope)),
            }
        }
        "getUniform" => {
            if !need(scope, args, 2) {
                return;
            }
            let program = obj_id(scope, args.get(0));
            let Some((_, name)) = loc_of(scope, args.get(1)) else {
                rv.set(null_val(scope));
                return;
            };
            let r = match ask(scope, this, GlQuery::Uniform { program, name }) {
                GlQueryResult::Floats(v) if v.len() == 1 => GlQueryResult::Float(v[0]),
                GlQueryResult::Ints(v) if v.len() == 1 => GlQueryResult::Int(v[0]),
                GlQueryResult::Bools(v) if v.len() == 1 => GlQueryResult::Bool(v[0]),
                GlQueryResult::UInts(v) if v.len() == 1 => GlQueryResult::UInt(v[0]),
                other => other,
            };
            set_result(scope, rv, r);
        }
        "getShaderPrecisionFormat" => {
            if !need(scope, args, 2) {
                return;
            }
            let shader_type = uint(scope, args, 0);
            let precision_type = uint(scope, args, 1);
            let shader = match shader_type {
                VERTEX_SHADER => "VERTEX_SHADER",
                FRAGMENT_SHADER => "FRAGMENT_SHADER",
                _ => "",
            };
            let precision_name = match precision_type {
                LOW_FLOAT => "LOW_FLOAT",
                MEDIUM_FLOAT => "MEDIUM_FLOAT",
                HIGH_FLOAT => "HIGH_FLOAT",
                0x8DF3 => "LOW_INT",
                0x8DF4 => "MEDIUM_INT",
                0x8DF5 => "HIGH_INT",
                _ => "",
            };
            // Blink checks both enums before asking GL (INVALID_ENUM and `null`).
            if shader.is_empty() || precision_name.is_empty() {
                rec!(scope, this, GlOp::RaiseError { code: INVALID_ENUM });
                rv.set(null_val(scope));
                return;
            }
            // The answer is the DRIVER's (ANGLE's GL backend passes `glGetShaderPrecisionFormat`
            // through, `renderergl_utils.cpp`), so it is the recorded device's: both recorded GPUs give
            // lowp/mediump float 15/15/10, which is not what this pipeline computes in (f32 throughout).
            // A bundle without the table answers the pipeline's own precision, as before.
            let recorded = recorded_precision(scope, &format!("{shader}.{precision_name}"));
            let (range_min, range_max, precision) = recorded.unwrap_or(match precision_type {
                HIGH_FLOAT | MEDIUM_FLOAT | LOW_FLOAT => (127, 127, 23),
                _ => (31, 30, 0),
            });
            let o = crate::install::live::mint_table(scope, "WebGLShaderPrecisionFormat");
            for (slot, v) in [("rangeMin", range_min), ("rangeMax", range_max), ("precision", precision)] {
                let n = int_val(scope, v as i64);
                set_slot(scope, o, slot, n);
            }
            rv.set(o.into());
        }

        // ── buffers ───────────────────────────────────────────────────────────────────────────
        "bindBuffer" => {
            if !need(scope, args, 2) {
                return;
            }
            let target = uint(scope, args, 0);
            let buffer = obj_id(scope, args.get(1));
            // every binding point, the WebGL 2 ones included: a GPU context needs them all (the software pipeline
            // reads only the vertex and index ones)
            rec!(scope, this, GlOp::BindBuffer { target, buffer });
            bound(scope, this, buffer);
        }
        "bufferData" => {
            if !need(scope, args, 2) {
                return;
            }
            let target = uint(scope, args, 0);
            let usage = uint(scope, args, 2);
            let src = args.get(1);
            let (data, size) = match raw_bytes(scope, src) {
                Some(data) => {
                    let n = data.len();
                    (data, n)
                }
                // The size-only form: `bufferData(target, 1024, usage)` allocates and zero-fills.
                None => (Vec::new(), num(scope, args, 1).max(0.0) as usize),
            };
            // Its contents, for `getBufferSubData` / `copyBufferSubData` (`gl2`).
            edit(scope, this, |s| gl2::buffer_data(s, target, &data, size));
            // A WebGL 2 binding point's buffer is validated here and its contents kept by the store above; the
            // backend gets it too, for a GPU context (the software pipeline reads only vertex and index buffers).
            if matches!(target, 0x8892 | 0x8893) {
                rec!(scope, this, GlOp::BufferData { target, size, data, usage });
            } else if edit(scope, this, |s| s.params.buffer_for(target)) == 0 {
                raise(scope, this, Some(if params::is_buffer_target(target) { INVALID_OPERATION } else { INVALID_ENUM }));
            } else {
                edit(scope, this, |s| s.ops.push(GlOp::BufferData { target, size, data, usage }));
            }
        }
        "bufferSubData" => {
            if !need(scope, args, 3) {
                return;
            }
            let target = uint(scope, args, 0);
            let offset = num(scope, args, 1).max(0.0) as usize;
            if let Some(data) = raw_bytes(scope, args.get(2)) {
                let e = edit(scope, this, |s| gl2::buffer_sub_data(s, target, offset, &data));
                if matches!(target, 0x8892 | 0x8893) {
                    rec!(scope, this, GlOp::BufferSubData { target, offset, data });
                } else if e.is_some() {
                    raise(scope, this, e);
                } else {
                    edit(scope, this, |s| s.ops.push(GlOp::BufferSubData { target, offset, data }));
                }
            }
        }
        "getBufferParameter" => {
            if !need(scope, args, 2) {
                return;
            }
            let target = uint(scope, args, 0);
            let pname = uint(scope, args, 1);
            let r = ask(scope, this, GlQuery::BufferParameter { target, pname });
            set_result(scope, rv, r);
        }

        // ── vertex attributes ─────────────────────────────────────────────────────────────────
        "enableVertexAttribArray" | "disableVertexAttribArray" => {
            if !need(scope, args, 1) {
                return;
            }
            let index = uint(scope, args, 0);
            rec!(
                scope,
                this,
                if member == "enableVertexAttribArray" {
                    GlOp::EnableVertexAttribArray { index }
                } else {
                    GlOp::DisableVertexAttribArray { index }
                },
            );
        }
        "vertexAttribPointer" => {
            if !need(scope, args, 6) {
                return;
            }
            let index = uint(scope, args, 0);
            edit(scope, this, |s| gl2b::note_float_pointer(s, index));
            rec!(
                scope,
                this,
                GlOp::VertexAttribPointer {
                    index: uint(scope, args, 0),
                    size: uint(scope, args, 1),
                    ty: uint(scope, args, 2),
                    normalized: boolean(scope, args, 3),
                    stride: uint(scope, args, 4),
                    offset: uint(scope, args, 5),
                },
            );
        }
        "vertexAttribIPointer" => {
            if !need(scope, args, 5) {
                return;
            }
            rec!(
                scope,
                this,
                GlOp::VertexAttribPointer {
                    index: uint(scope, args, 0),
                    size: uint(scope, args, 1),
                    ty: uint(scope, args, 2),
                    normalized: false,
                    stride: uint(scope, args, 3),
                    offset: uint(scope, args, 4),
                },
            );
        }
        "vertexAttribDivisor" => {
            if !need(scope, args, 2) {
                return;
            }
            rec!(
                scope,
                this,
                GlOp::VertexAttribDivisor { index: uint(scope, args, 0), divisor: uint(scope, args, 1) },
            );
        }
        _ if member.starts_with("vertexAttrib") && (member.ends_with('f') || member.ends_with("fv")) => {
            // vertexAttrib{1,2,3,4}f[v] — one op, the six spellings normalized.
            if !need(scope, args, 2) {
                return;
            }
            let index = uint(scope, args, 0);
            let vals: Vec<f32> = if member.ends_with("fv") {
                f32_seq(scope, args.get(1))
            } else {
                let n = member
                    .trim_start_matches("vertexAttrib")
                    .trim_end_matches('f')
                    .parse::<i32>()
                    .unwrap_or(1);
                (0..n).map(|i| flt(scope, args, 1 + i)).collect()
            };
            let mut values = [0.0f32, 0.0, 0.0, 1.0];
            for (i, v) in vals.iter().take(4).enumerate() {
                values[i] = *v;
            }
            edit(scope, this, |s| gl2b::note_float_value(s, index));
            rec!(scope, this, GlOp::VertexAttrib { index, values });
        }
        _ if member.starts_with("vertexAttribI4") => {
            if !need(scope, args, 2) {
                return;
            }
            let index = uint(scope, args, 0);
            let vals: Vec<f32> = if member.ends_with('v') {
                f32_seq(scope, args.get(1))
            } else {
                (0..4).map(|i| flt(scope, args, 1 + i)).collect()
            };
            let mut values = [0.0f32, 0.0, 0.0, 1.0];
            for (i, v) in vals.iter().take(4).enumerate() {
                values[i] = *v;
            }
            rec!(scope, this, GlOp::VertexAttrib { index, values });
        }
        "getVertexAttrib" => {
            if !need(scope, args, 2) {
                return;
            }
            let index = uint(scope, args, 0);
            let pname = uint(scope, args, 1);
            if gl2b::vertex_attrib(scope, this, index, pname, iface == "WebGL2RenderingContext", rv) {
                return;
            }
            // The buffer-binding query answers with the OBJECT the page created, not its id.
            if pname == 0x889F {
                return match ask(scope, this, GlQuery::VertexAttrib { index, pname }) {
                    GlQueryResult::UInt(id) if id != 0 => match object_of(scope, this, id).or_else(|| new_gl_object(scope, "WebGLBuffer", id)) {
                        Some(o) => rv.set(o),
                        None => rv.set(null_val(scope)),
                    },
                    _ => rv.set(null_val(scope)),
                };
            }
            let r = ask(scope, this, GlQuery::VertexAttrib { index, pname });
            set_result(scope, rv, r);
        }
        "getVertexAttribOffset" => {
            if !need(scope, args, 2) {
                return;
            }
            let index = uint(scope, args, 0);
            let pname = uint(scope, args, 1);
            let r = ask(scope, this, GlQuery::VertexAttribOffset { index, pname });
            set_result(scope, rv, r);
        }

        // ── uniforms. Twenty-eight spellings, one op. ─────────────────────────────────────────
        _ if member.starts_with("uniform") && member != "uniformBlockBinding" => {
            if !need(scope, args, 2) {
                return;
            }
            let Some((program, name)) = loc_of(scope, args.get(0)) else {
                // A null location is a documented no-op, not an error: `uniform1f(null, x)` is how
                // a page skips a uniform the linker optimized away.
                return;
            };
            let value = match parse_uniform(scope, args, &member) {
                Some(v) => v,
                None => return,
            };
            let Some(value) = gl2b::check_uniform(scope, this, &member, args, program, &name, value, iface == "WebGL2RenderingContext") else { return };
            rec!(scope, this, GlOp::Uniform { program, name, value });
        }

        // ── textures ──────────────────────────────────────────────────────────────────────────
        "activeTexture" => {
            if !need(scope, args, 1) {
                return;
            }
            rec!(scope, this, GlOp::ActiveTexture { unit: uint(scope, args, 0) });
        }
        "bindTexture" => {
            if !need(scope, args, 2) {
                return;
            }
            let target = uint(scope, args, 0);
            let texture = obj_id(scope, args.get(1));
            // 3D and array textures too: the software pipeline never samples them, a GPU context does
            rec!(scope, this, GlOp::BindTexture { target, texture });
            bound(scope, this, texture);
        }
        "texImage2D" => {
            if !need(scope, args, 6) {
                return;
            }
            let target = uint(scope, args, 0);
            let level = int(scope, args, 1);
            let internal_format = uint(scope, args, 2);
            let flip = edit(scope, this, |s| {
                s.ops.iter().rev().find_map(|o| match o {
                    GlOp::PixelStorei { pname, param } if *pname == UNPACK_FLIP_Y_WEBGL => Some(*param != 0),
                    _ => None,
                })
            })
            .unwrap_or(false);
            let (width, height, format, ty, pixels) = if args.length() >= 8 {
                // texImage2D(target, level, internalformat, width, height, border, format, type, src)
                let w = uint(scope, args, 3);
                let h = uint(scope, args, 4);
                let format = uint(scope, args, 6);
                let ty = uint(scope, args, 7);
                let px = raw_bytes(scope, args.get(8)).and_then(|b| decode_texels(&b, w, h, format, ty));
                (w, h, format, ty, px)
            } else {
                // texImage2D(target, level, internalformat, format, type, source)
                let format = uint(scope, args, 3);
                let ty = uint(scope, args, 4);
                let Ok(px) = super::canvas_image::tex_source(scope, args.get(5), iface, "texImage2D") else { return };
                let (w, h) = px.as_ref().map(|p| (p.width, p.height)).unwrap_or((0, 0));
                (w, h, format, ty, px)
            };
            let pixels = pixels.map(|p| if flip { flip_rows(p) } else { p });
            if let Some(e) = edit(scope, this, |s| gl2::tex_image(s, target, level, width, height)) {
                raise(scope, this, Some(e));
                return;
            }
            edit(scope, this, |s| gl2b::note_tex_level(s, target, level, internal_format, ty));
            rec!(
                scope,
                this,
                GlOp::TexImage2D { target, level, internal_format, width, height, format, ty, pixels },
            );
        }
        "texSubImage2D" => {
            if !need(scope, args, 7) {
                return;
            }
            let target = uint(scope, args, 0);
            let level = int(scope, args, 1);
            let xoffset = int(scope, args, 2);
            let yoffset = int(scope, args, 3);
            let (format, ty, pixels) = if args.length() >= 8 {
                let w = uint(scope, args, 4);
                let h = uint(scope, args, 5);
                let format = uint(scope, args, 6);
                let ty = uint(scope, args, 7);
                (format, ty, raw_bytes(scope, args.get(8)).and_then(|b| decode_texels(&b, w, h, format, ty)))
            } else {
                let format = uint(scope, args, 4);
                let ty = uint(scope, args, 5);
                let Ok(px) = super::canvas_image::tex_source(scope, args.get(6), iface, "texSubImage2D") else { return };
                (format, ty, px)
            };
            if let Some(pixels) = pixels {
                let (w, h) = (pixels.width as i64, pixels.height as i64);
                if let Some(e) = edit(scope, this, |s| gl2::tex_sub_image(s, target, level, xoffset as i64, yoffset as i64, w, h)) {
                    raise(scope, this, Some(e));
                    return;
                }
                rec!(scope, this, GlOp::TexSubImage2D { target, level, xoffset, yoffset, format, ty, pixels });
            }
        }
        "texParameteri" | "texParameterf" => {
            if !need(scope, args, 3) {
                return;
            }
            rec!(
                scope,
                this,
                GlOp::TexParameter {
                    target: uint(scope, args, 0),
                    pname: uint(scope, args, 1),
                    param: flt(scope, args, 2),
                },
            );
        }
        "getTexParameter" => {
            if !need(scope, args, 2) {
                return;
            }
            let target = uint(scope, args, 0);
            let pname = uint(scope, args, 1);
            if iface == "WebGL2RenderingContext" {
                match edit(scope, this, |s| gl2::immutable_param(s, target, pname)) {
                    Some(Ok(b)) => return rv.set_bool(b),
                    Some(Err(n)) => return rv.set_uint32(n),
                    None => {}
                }
            }
            let r = ask(scope, this, GlQuery::TexParameter { target, pname });
            set_result(scope, rv, r);
        }
        "generateMipmap" => {
            if !need(scope, args, 1) {
                return;
            }
            rec!(scope, this, GlOp::GenerateMipmap { target: uint(scope, args, 0) });
        }
        "pixelStorei" => {
            if !need(scope, args, 2) {
                return;
            }
            rec!(
                scope,
                this,
                GlOp::PixelStorei { pname: uint(scope, args, 0), param: int(scope, args, 1) },
            );
        }

        // ── framebuffers ──────────────────────────────────────────────────────────────────────
        "bindFramebuffer" => {
            if !need(scope, args, 2) {
                return;
            }
            let target = uint(scope, args, 0);
            let framebuffer = obj_id(scope, args.get(1));
            // READ_FRAMEBUFFER too: it decides what a GPU context's readPixels and blits read (the software
            // pipeline draws into, and reads, its one binding)
            rec!(scope, this, GlOp::BindFramebuffer { target, framebuffer });
            bound(scope, this, framebuffer);
        }
        "bindRenderbuffer" => {
            if !need(scope, args, 2) {
                return;
            }
            let target = uint(scope, args, 0);
            let renderbuffer = obj_id(scope, args.get(1));
            rec!(scope, this, GlOp::BindRenderbuffer { target, renderbuffer });
            bound(scope, this, renderbuffer);
        }
        "renderbufferStorage" | "renderbufferStorageMultisample" => {
            let (base, n) = if member == "renderbufferStorage" { (0, 4) } else { (1, 5) };
            if !need(scope, args, n) {
                return;
            }
            rec!(
                scope,
                this,
                GlOp::RenderbufferStorage {
                    target: uint(scope, args, 0),
                    internal_format: uint(scope, args, 1 + base),
                    width: uint(scope, args, 2 + base),
                    height: uint(scope, args, 3 + base),
                },
            );
            let internal = uint(scope, args, 1 + base);
            edit(scope, this, |s| gl2b::note_renderbuffer(s, internal));
        }
        "framebufferTexture2D" => {
            if !need(scope, args, 5) {
                return;
            }
            let texture = obj_id(scope, args.get(3));
            rec!(
                scope,
                this,
                GlOp::FramebufferTexture2D {
                    target: uint(scope, args, 0),
                    attachment: uint(scope, args, 1),
                    textarget: uint(scope, args, 2),
                    texture,
                    level: int(scope, args, 4),
                },
            );
            let (target, attachment, textarget, level) = (uint(scope, args, 0), uint(scope, args, 1), uint(scope, args, 2), int(scope, args, 4));
            edit(scope, this, |s| gl2b::note_attach(s, target, attachment, gl2b::texture_attach(texture, level, textarget)));
        }
        "framebufferRenderbuffer" => {
            if !need(scope, args, 4) {
                return;
            }
            let renderbuffer = obj_id(scope, args.get(3));
            rec!(
                scope,
                this,
                GlOp::FramebufferRenderbuffer {
                    target: uint(scope, args, 0),
                    attachment: uint(scope, args, 1),
                    rb_target: uint(scope, args, 2),
                    renderbuffer,
                },
            );
            let (target, attachment) = (uint(scope, args, 0), uint(scope, args, 1));
            edit(scope, this, |s| gl2b::note_attach(s, target, attachment, gl2b::renderbuffer_attach(renderbuffer)));
        }
        "checkFramebufferStatus" => {
            if !need(scope, args, 1) {
                return;
            }
            let target = uint(scope, args, 0);
            if let Some(status) = edit(scope, this, |s| gl2b::layered_status(s, target)) {
                rv.set_uint32(status);
                return;
            }
            let r = ask(scope, this, GlQuery::FramebufferStatus { target });
            set_result(scope, rv, r);
        }
        "getRenderbufferParameter" => {
            if !need(scope, args, 2) {
                return;
            }
            let target = uint(scope, args, 0);
            let pname = uint(scope, args, 1);
            let r = ask(scope, this, GlQuery::RenderbufferParameter { target, pname });
            set_result(scope, rv, r);
        }
        "getFramebufferAttachmentParameter" => {
            if !need(scope, args, 3) {
                return;
            }
            rv.set(null_val(scope));
        }

        // ── fixed-function state ──────────────────────────────────────────────────────────────
        "viewport" | "scissor" => {
            if !need(scope, args, 4) {
                return;
            }
            let (x, y, w, h) = (int(scope, args, 0), int(scope, args, 1), int(scope, args, 2), int(scope, args, 3));
            rec!(
                scope,
                this,
                if member == "viewport" {
                    GlOp::Viewport { x, y, width: w, height: h }
                } else {
                    GlOp::Scissor { x, y, width: w, height: h }
                },
            );
        }
        "clearColor" | "blendColor" => {
            if !need(scope, args, 4) {
                return;
            }
            let (r, g, b, a) = (flt(scope, args, 0), flt(scope, args, 1), flt(scope, args, 2), flt(scope, args, 3));
            rec!(
                scope,
                this,
                if member == "clearColor" {
                    GlOp::ClearColor { r, g, b, a }
                } else {
                    GlOp::BlendColor { r, g, b, a }
                },
            );
        }
        "clearDepth" => {
            if !need(scope, args, 1) {
                return;
            }
            rec!(scope, this, GlOp::ClearDepth { depth: flt(scope, args, 0) });
        }
        "clearStencil" => {
            if !need(scope, args, 1) {
                return;
            }
            rec!(scope, this, GlOp::ClearStencil { s: int(scope, args, 0) });
        }
        "clear" => {
            if !need(scope, args, 1) {
                return;
            }
            rec!(scope, this, GlOp::Clear { mask: uint(scope, args, 0) });
        }
        "colorMask" => {
            if !need(scope, args, 4) {
                return;
            }
            rec!(
                scope,
                this,
                GlOp::ColorMask {
                    r: boolean(scope, args, 0),
                    g: boolean(scope, args, 1),
                    b: boolean(scope, args, 2),
                    a: boolean(scope, args, 3),
                },
            );
        }
        "depthMask" => {
            if !need(scope, args, 1) {
                return;
            }
            rec!(scope, this, GlOp::DepthMask { flag: boolean(scope, args, 0) });
        }
        "depthFunc" => {
            if !need(scope, args, 1) {
                return;
            }
            rec!(scope, this, GlOp::DepthFunc { func: uint(scope, args, 0) });
        }
        "depthRange" => {
            if !need(scope, args, 2) {
                return;
            }
            rec!(scope, this, GlOp::DepthRange { near: flt(scope, args, 0), far: flt(scope, args, 1) });
        }
        "stencilMask" => {
            if !need(scope, args, 1) {
                return;
            }
            rec!(scope, this, GlOp::StencilMask { mask: uint(scope, args, 0) });
        }
        "stencilMaskSeparate" => {
            if !need(scope, args, 2) {
                return;
            }
            // One face's write mask. The backend allocates no stencil buffer, so this is state only.
            let (face, mask) = (uint(scope, args, 0), uint(scope, args, 1));
            let e = edit(scope, this, |s| s.params.stencil_mask_separate(face, mask));
            raise(scope, this, e);
        }
        "stencilFunc" | "stencilFuncSeparate" => {
            let sep = member.ends_with("Separate");
            if !need(scope, args, if sep { 4 } else { 3 }) {
                return;
            }
            let b = if sep { 1 } else { 0 };
            rec!(
                scope,
                this,
                GlOp::StencilFunc {
                    face: if sep { uint(scope, args, 0) } else { FRONT_AND_BACK },
                    func: uint(scope, args, b),
                    reference: int(scope, args, b + 1),
                    mask: uint(scope, args, b + 2),
                },
            );
        }
        "stencilOp" | "stencilOpSeparate" => {
            let sep = member.ends_with("Separate");
            if !need(scope, args, if sep { 4 } else { 3 }) {
                return;
            }
            let b = if sep { 1 } else { 0 };
            rec!(
                scope,
                this,
                GlOp::StencilOp {
                    face: if sep { uint(scope, args, 0) } else { FRONT_AND_BACK },
                    fail: uint(scope, args, b),
                    zfail: uint(scope, args, b + 1),
                    zpass: uint(scope, args, b + 2),
                },
            );
        }
        "enable" | "disable" => {
            if !need(scope, args, 1) {
                return;
            }
            let cap = uint(scope, args, 0);
            rec!(scope, this, if member == "enable" { GlOp::Enable { cap } } else { GlOp::Disable { cap } });
        }
        "isEnabled" => {
            if !need(scope, args, 1) {
                return;
            }
            let cap = uint(scope, args, 0);
            // The same capability set `enable` writes and `getParameter` reads; a name `enable` would
            // refuse is INVALID_ENUM here too.
            let (valid, on) = edit(scope, this, |s| (params::Params::valid_cap(cap, s.v2), s.params.is_enabled(cap)));
            if !valid {
                rec!(scope, this, GlOp::RaiseError { code: INVALID_ENUM });
            }
            rv.set_bool(valid && on);
        }
        "blendFunc" => {
            if !need(scope, args, 2) {
                return;
            }
            let (s, d) = (uint(scope, args, 0), uint(scope, args, 1));
            rec!(scope, this, GlOp::BlendFuncSeparate { src_rgb: s, dst_rgb: d, src_alpha: s, dst_alpha: d });
        }
        "blendFuncSeparate" => {
            if !need(scope, args, 4) {
                return;
            }
            rec!(
                scope,
                this,
                GlOp::BlendFuncSeparate {
                    src_rgb: uint(scope, args, 0),
                    dst_rgb: uint(scope, args, 1),
                    src_alpha: uint(scope, args, 2),
                    dst_alpha: uint(scope, args, 3),
                },
            );
        }
        "blendEquation" => {
            if !need(scope, args, 1) {
                return;
            }
            let m = uint(scope, args, 0);
            rec!(scope, this, GlOp::BlendEquationSeparate { rgb: m, alpha: m });
        }
        "blendEquationSeparate" => {
            if !need(scope, args, 2) {
                return;
            }
            rec!(
                scope,
                this,
                GlOp::BlendEquationSeparate { rgb: uint(scope, args, 0), alpha: uint(scope, args, 1) },
            );
        }
        "cullFace" => {
            if !need(scope, args, 1) {
                return;
            }
            rec!(scope, this, GlOp::CullFace { mode: uint(scope, args, 0) });
        }
        "frontFace" => {
            if !need(scope, args, 1) {
                return;
            }
            rec!(scope, this, GlOp::FrontFace { mode: uint(scope, args, 0) });
        }
        "lineWidth" => {
            if !need(scope, args, 1) {
                return;
            }
            rec!(scope, this, GlOp::LineWidth { width: flt(scope, args, 0) });
        }
        "polygonOffset" => {
            if !need(scope, args, 2) {
                return;
            }
            rec!(
                scope,
                this,
                GlOp::PolygonOffset { factor: flt(scope, args, 0), units: flt(scope, args, 1) },
            );
        }
        "sampleCoverage" => {
            if !need(scope, args, 2) {
                return;
            }
            rec!(
                scope,
                this,
                GlOp::SampleCoverage { value: flt(scope, args, 0), invert: boolean(scope, args, 1) },
            );
        }
        "hint" => {
            if !need(scope, args, 2) {
                return;
            }
            rec!(scope, this, GlOp::Hint { target: uint(scope, args, 0), mode: uint(scope, args, 1) });
        }
        "finish" => rec!(scope, this, GlOp::Finish),
        "flush" => rec!(scope, this, GlOp::Flush),

        // ── draw and readback ─────────────────────────────────────────────────────────────────
        "drawArrays" => {
            if !need(scope, args, 3) {
                return;
            }
            rec!(
                scope,
                this,
                GlOp::DrawArrays {
                    mode: uint(scope, args, 0),
                    first: int(scope, args, 1),
                    count: int(scope, args, 2),
                },
            );
        }
        "drawArraysInstanced" => {
            if !need(scope, args, 4) {
                return;
            }
            rec!(
                scope,
                this,
                GlOp::DrawArraysInstanced {
                    mode: uint(scope, args, 0),
                    first: int(scope, args, 1),
                    count: int(scope, args, 2),
                    instances: int(scope, args, 3),
                },
            );
        }
        "drawElements" => {
            if !need(scope, args, 4) {
                return;
            }
            rec!(
                scope,
                this,
                GlOp::DrawElements {
                    mode: uint(scope, args, 0),
                    count: int(scope, args, 1),
                    ty: uint(scope, args, 2),
                    offset: uint(scope, args, 3),
                },
            );
        }
        "drawElementsInstanced" => {
            if !need(scope, args, 5) {
                return;
            }
            rec!(
                scope,
                this,
                GlOp::DrawElementsInstanced {
                    mode: uint(scope, args, 0),
                    count: int(scope, args, 1),
                    ty: uint(scope, args, 2),
                    offset: uint(scope, args, 3),
                    instances: int(scope, args, 4),
                },
            );
        }
        "drawRangeElements" => {
            if !need(scope, args, 6) {
                return;
            }
            rec!(
                scope,
                this,
                GlOp::DrawElements {
                    mode: uint(scope, args, 0),
                    count: int(scope, args, 3),
                    ty: uint(scope, args, 4),
                    offset: uint(scope, args, 5),
                },
            );
        }
        "readPixels" => {
            if !need(scope, args, 7) {
                return;
            }
            let x = int(scope, args, 0);
            let y = int(scope, args, 1);
            let width = uint(scope, args, 2);
            let height = uint(scope, args, 3);
            let format = uint(scope, args, 4);
            let ty = uint(scope, args, 5);
            let dest = args.get(6);
            let Ok(dest) = v8::Local::<v8::Object>::try_from(dest) else { return };
            let bytes = if plugin_host::has_readback(plugin_host::READBACK_WEBGL) { read_pixels_through_plugins(scope, this, x, y, width, height) } else { None };
            let bytes = bytes.unwrap_or_else(|| match ask(scope, this, GlQuery::ReadPixels { x, y, width, height }) {
                GlQueryResult::Bytes(b) => b,
                _ => vec![0u8; (width as usize) * (height as usize) * 4],
            });
            write_pixels(scope, dest, &bytes, width, height, format, ty);
        }
        "getError" => {
            if let Some(e) = edit(scope, this, |s| (!s.synthetic.is_empty()).then(|| s.synthetic.remove(0))) {
                rv.set_uint32(e);
                return;
            }
            let consumed = edit(scope, this, |s| s.errors_read);
            let e = match ask(scope, this, GlQuery::Error { consumed }) {
                GlQueryResult::UInt(e) => e,
                _ => 0,
            };
            if e != 0 {
                edit(scope, this, |s| s.errors_read += 1);
            }
            rv.set_uint32(e);
        }
        "isContextLost" => rv.set_bool(false),
        "makeXRCompatible" => {
            // A promise, because that is the IDL return type; rejected, because no XR device exists
            // and resolving would claim one.
            let msg = str_val(scope, "NotSupportedError");
            let p = crate::binding::protos::rejected_promise(scope, msg);
            rv.set(p);
        }

        // ── the WebGL 2 members this pipeline does not model. They are REAL no-ops here, not
        // recorded-and-dropped ops: nothing downstream consumes a transform feedback or an
        // occlusion query, so recording one would claim a fidelity the pixels do not have. ──
        "beginQuery" | "bindTransformFeedback" => {
            // No effect on pixels (see below), and still the moment the object BECOMES one:
            // `isQuery` / `isTransformFeedback` are false until then (measured).
            let id = obj_id(scope, args.get(1));
            bound(scope, this, id);
            if member == "bindTransformFeedback" {
                edit(scope, this, |s| s.params.bind_transform_feedback(id));
            }
        }
        // The WebGL 2 binding points and buffer selections `getParameter` reads back. They draw
        // nothing here (see below), and they are still state a page can read.
        "bindSampler" => {
            if !need(scope, args, 2) {
                return;
            }
            let (unit, sampler) = (uint(scope, args, 0), obj_id(scope, args.get(1)));
            let e = edit(scope, this, |s| s.params.bind_sampler(unit, sampler));
            raise(scope, this, e);
        }
        "bindBufferBase" | "bindBufferRange" => {
            if !need(scope, args, if member == "bindBufferBase" { 3 } else { 5 }) {
                return;
            }
            let (target, buffer) = (uint(scope, args, 0), obj_id(scope, args.get(2)));
            let e = edit(scope, this, |s| s.params.bind_buffer_base(target, buffer));
            raise(scope, this, e);
            bound(scope, this, buffer);
        }
        "readBuffer" => {
            if !need(scope, args, 1) {
                return;
            }
            let src = uint(scope, args, 0);
            let e = edit(scope, this, |s| s.params.read_buffer(src));
            raise(scope, this, e);
        }
        "drawBuffers" => {
            if !need(scope, args, 1) {
                return;
            }
            let list: Vec<u32> = i32_seq(scope, args.get(0)).into_iter().map(|v| v as u32).collect();
            let e = edit(scope, this, |s| s.params.draw_buffers(list));
            raise(scope, this, e);
        }
        "endQuery" | "beginTransformFeedback"
        | "endTransformFeedback" | "pauseTransformFeedback" | "resumeTransformFeedback"
        | "samplerParameteri" | "samplerParameterf"
        | "transformFeedbackVaryings" | "uniformBlockBinding" | "invalidateFramebuffer"
        | "invalidateSubFramebuffer" | "waitSync"
        | "drawingBufferStorage" => {}
        "bindVertexArray" => {
            if !need(scope, args, 1) {
                return;
            }
            let id = obj_id(scope, args.get(0));
            rec!(scope, this, GlOp::BindVertexArray { id });
            bound(scope, this, id);
        }
        "clientWaitSync" => rv.set_uint32(0x911A), // ALREADY_SIGNALED — nothing is ever pending here
        "getSyncParameter" | "getQueryParameter" | "getQuery" | "getSamplerParameter"
        | "getIndexedParameter" | "getActiveUniforms"
        | "getActiveUniformBlockParameter" | "getActiveUniformBlockName"
        | "getTransformFeedbackVarying" => rv.set(null_val(scope)),
        // Blink's validation of the format (`WebGL2RenderingContextBase::getInternalformatParameter`),
        // then GL's sample counts: the recorded device's, or derived from its MAX_SAMPLES.
        "getInternalformatParameter" => {
            if !need(scope, args, 3) {
                return;
            }
            let (target, format, pname) = (uint(scope, args, 0), uint(scope, args, 1), uint(scope, args, 2));
            if target != 0x8D41 {
                rec!(scope, this, GlOp::RaiseError { code: INVALID_ENUM });
                rv.set(null_val(scope));
                return;
            }
            let class = edit(scope, this, |s| params::internalformat_class(format, &s.enabled));
            let counts: Vec<i32> = match class {
                Err(()) => {
                    rec!(scope, this, GlOp::RaiseError { code: INVALID_ENUM });
                    rv.set(null_val(scope));
                    return;
                }
                // The unsized and integer formats answer an empty list before `pname` is looked at.
                Ok(false) => {
                    if let Some(a) = new_i32_array(scope, &[]) {
                        rv.set(a);
                    }
                    return;
                }
                Ok(true) => match recorded_samples(scope, format) {
                    Some(v) => v,
                    None => params::derived_samples(device_number(scope, 0x8D57, true).unwrap_or(4.0) as u32),
                },
            };
            if pname != 0x80A9 {
                rec!(scope, this, GlOp::RaiseError { code: INVALID_ENUM });
                rv.set(null_val(scope));
                return;
            }
            // `glGetInternalformativ`: a GPU-process round trip in Chrome, asked of the GPU here
            let _ = ask(scope, this, GlQuery::Internalformat { target, internal_format: format, pname });
            match new_i32_array(scope, &counts) {
                Some(a) => rv.set(a),
                None => rv.set(null_val(scope)),
            }
        }
        "getUniformBlockIndex" | "getFragDataLocation" => rv.set_int32(-1),
        "getUniformIndices" => rv.set(null_val(scope)),

        other => {
            // Reached only if `install` names a member this dispatcher does not handle — a wiring
            // bug, and one that must be loud rather than a silent no-op.
            throw_type_error(
                scope,
                &format!("Failed to execute '{other}' on '{iface}': not implemented by this engine."),
            );
        }
    }
}

/// `uniform*` in all its spellings -> one [`GlUniformValue`].
fn parse_uniform(
    scope: &mut v8::PinScope,
    args: &v8::FunctionCallbackArguments,
    member: &str,
) -> Option<GlUniformValue> {
    let rest = member.strip_prefix("uniform")?;
    if let Some(m) = rest.strip_prefix("Matrix") {
        // Matrix2fv / Matrix3x4fv / ...
        let dims = m.trim_end_matches("fv");
        let (cols, rows) = match dims.split_once('x') {
            Some((c, r)) => (c.parse::<u8>().ok()?, r.parse::<u8>().ok()?),
            None => {
                let n = dims.parse::<u8>().ok()?;
                (n, n)
            }
        };
        let transpose = args.get(1).boolean_value(scope);
        let values = f32_seq(scope, args.get(2));
        return Some(GlUniformValue::Matrix { rows, cols, transpose, values });
    }
    let components: u8 = rest.chars().next()?.to_digit(10)? as u8;
    let kind = &rest[1..];
    Some(match kind {
        "f" => GlUniformValue::Float {
            components,
            values: (0..components as i32).map(|i| flt(scope, args, 1 + i)).collect(),
        },
        "fv" => GlUniformValue::Float { components, values: f32_seq(scope, args.get(1)) },
        "i" => GlUniformValue::Int {
            components,
            values: (0..components as i32).map(|i| int(scope, args, 1 + i)).collect(),
        },
        "iv" => GlUniformValue::Int { components, values: i32_seq(scope, args.get(1)) },
        "ui" => GlUniformValue::UInt {
            components,
            values: (0..components as i32).map(|i| uint(scope, args, 1 + i)).collect(),
        },
        "uiv" => GlUniformValue::UInt {
            components,
            values: i32_seq(scope, args.get(1)).iter().map(|&v| v as u32).collect(),
        },
        _ => return None,
    })
}

/// Copy `readPixels` bytes into the page's view, converting to the requested format.
/// `readPixels(x, y, width, height)` read back through the plugins that fill the readback slot
/// (`plugin_host::readback_rgba`, ABI 2).
///
/// From the drawing buffer, the plugins see the WHOLE buffer, top-left origin -- the positions `toDataURL` hands them
/// -- so two reads of overlapping rectangles agree where they overlap and a pixel reads as the canvas exports it: the
/// buffer is read whole (GL's rows, bottom-up), handed over top-down, and the rectangle cut from it; what lies outside
/// the buffer stays zero. From a framebuffer object, which nothing else reads back, the rectangle is handed over as it
/// is read. `None` when the device gave no bytes.
fn read_pixels_through_plugins(
    scope: &mut v8::PinScope,
    ctx: v8::Local<v8::Object>,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Option<Vec<u8>> {
    let flip = |data: &[u8], w: usize, h: usize| -> Vec<u8> {
        let mut out = vec![0u8; data.len()];
        for r in 0..h {
            out[r * w * 4..(r + 1) * w * 4].copy_from_slice(&data[(h - 1 - r) * w * 4..(h - r) * w * 4]);
        }
        out
    };
    if edit(scope, ctx, |s| s.params.read_framebuffer()) != 0 {
        let GlQueryResult::Bytes(b) = ask(scope, ctx, GlQuery::ReadPixels { x, y, width, height }) else { return None };
        let (w, h) = (width as usize, height as usize);
        if b.len() != w * h * 4 {
            return Some(b);
        }
        let mut img = Rgba8 { width, height, data: flip(&b, w, h) };
        plugin_host::readback_rgba(plugin_host::READBACK_WEBGL, img.width, img.height, &mut img.data);
        return Some(flip(&img.data, w, h));
    }
    let bw = drawing_buffer_extent(scope, ctx, "width")?;
    let bh = drawing_buffer_extent(scope, ctx, "height")?;
    let GlQueryResult::Bytes(b) = ask(scope, ctx, GlQuery::ReadPixels { x: 0, y: 0, width: bw, height: bh }) else { return None };
    let (fw, fh) = (bw as usize, bh as usize);
    if b.len() != fw * fh * 4 {
        return None;
    }
    let mut full = Rgba8 { width: bw, height: bh, data: flip(&b, fw, fh) };
    plugin_host::readback_rgba(plugin_host::READBACK_WEBGL, full.width, full.height, &mut full.data);
    let mut out = vec![0u8; (width as usize) * (height as usize) * 4];
    for r in 0..height as i64 {
        let gy = y as i64 + r;
        if gy < 0 || gy >= bh as i64 {
            continue;
        }
        let ty = (bh as i64 - 1 - gy) as usize;
        for c in 0..width as i64 {
            let gx = x as i64 + c;
            if gx < 0 || gx >= bw as i64 {
                continue;
            }
            let (si, di) = ((ty * fw + gx as usize) * 4, ((r * width as i64 + c) * 4) as usize);
            out[di..di + 4].copy_from_slice(&full.data[si..si + 4]);
        }
    }
    Some(out)
}

fn write_pixels(
    scope: &mut v8::PinScope,
    dest: v8::Local<v8::Object>,
    rgba: &[u8],
    width: u32,
    height: u32,
    format: u32,
    ty: u32,
) {
    let n = (width as usize) * (height as usize);
    let comps = match format {
        RGB => 3usize,
        ALPHA | LUMINANCE => 1,
        LUMINANCE_ALPHA => 2,
        _ => 4,
    };
    for i in 0..n {
        let src = &rgba[i * 4..i * 4 + 4];
        let out: [u8; 4] = match format {
            RGB => [src[0], src[1], src[2], 0],
            ALPHA => [src[3], 0, 0, 0],
            LUMINANCE => [src[0], 0, 0, 0],
            LUMINANCE_ALPHA => [src[0], src[3], 0, 0],
            _ => [src[0], src[1], src[2], src[3]],
        };
        for k in 0..comps {
            let idx = (i * comps + k) as u32;
            let v: v8::Local<v8::Value> = match ty {
                UNSIGNED_BYTE => int_val(scope, out[k] as i64),
                // A float readback normalizes, which is what a FLOAT/HALF_FLOAT colour buffer holds.
                _ => v8::Number::new(scope, out[k] as f64 / 255.0).into(),
            };
            dest.set_index(scope, idx, v);
        }
    }
}

/// Turn a backend answer into the JS value WebGL's IDL says the member returns.
fn set_result(scope: &mut v8::PinScope, rv: &mut v8::ReturnValue, r: GlQueryResult) {
    match r {
        GlQueryResult::None => {
            let n = v8::null(scope);
            rv.set(n.into());
        }
        GlQueryResult::Bool(b) => rv.set_bool(b),
        GlQueryResult::Int(i) => rv.set_int32(i),
        GlQueryResult::UInt(u) => rv.set_uint32(u),
        GlQueryResult::Float(f) => rv.set_double(f as f64),
        GlQueryResult::Str(s) => {
            let v = str_val(scope, &s);
            rv.set(v);
        }
        GlQueryResult::Floats(v) => {
            // `getUniform` on a vector returns a Float32Array in every browser, not a plain Array.
            match new_f32_array(scope, &v) {
                Some(a) => rv.set(a),
                None => rv.set_undefined(),
            }
        }
        GlQueryResult::Ints(v) => match new_i32_array(scope, &v) {
            Some(a) => rv.set(a),
            None => rv.set_undefined(),
        },
        GlQueryResult::Bools(v) => {
            let arr = v8::Array::new(scope, v.len() as i32);
            for (i, b) in v.iter().enumerate() {
                let bv = v8::Boolean::new(scope, *b);
                arr.set_index(scope, i as u32, bv.into());
            }
            rv.set(arr.into());
        }
        GlQueryResult::UInts(v) => match new_u32_array(scope, &v) {
            Some(a) => rv.set(a),
            None => rv.set_undefined(),
        },
        GlQueryResult::ActiveInfo { .. } | GlQueryResult::Bytes(_) => rv.set_undefined(),
    }
}

/// A `Uint32Array` over `v` -- a `uvec*` uniform's value (`getUniform`), a uniform block's indices.
pub(crate) fn new_u32_array<'s>(scope: &mut v8::PinScope<'s, '_>, v: &[u32]) -> Option<v8::Local<'s, v8::Value>> {
    let ab = v8::ArrayBuffer::new(scope, v.len() * 4);
    let arr = v8::Uint32Array::new(scope, ab, 0, v.len())?;
    for (i, x) in v.iter().enumerate() {
        let n = v8::Number::new(scope, *x as f64);
        arr.set_index(scope, i as u32, n.into());
    }
    Some(arr.into())
}

fn new_f32_array<'s>(scope: &mut v8::PinScope<'s, '_>, v: &[f32]) -> Option<v8::Local<'s, v8::Value>> {
    let ab = v8::ArrayBuffer::new(scope, v.len() * 4);
    let arr = v8::Float32Array::new(scope, ab, 0, v.len())?;
    for (i, x) in v.iter().enumerate() {
        let n = v8::Number::new(scope, *x as f64);
        arr.set_index(scope, i as u32, n.into());
    }
    Some(arr.into())
}

fn new_i32_array<'s>(scope: &mut v8::PinScope<'s, '_>, v: &[i32]) -> Option<v8::Local<'s, v8::Value>> {
    let ab = v8::ArrayBuffer::new(scope, v.len() * 4);
    let arr = v8::Int32Array::new(scope, ab, 0, v.len())?;
    for (i, x) in v.iter().enumerate() {
        let n = int_val(scope, *x as i64);
        arr.set_index(scope, i as u32, n);
    }
    Some(arr.into())
}

// ── install ───────────────────────────────────────────────────────────────────────────────────

// THE TWO `(member, length)` ARITY TABLES THAT USED TO LIVE HERE ARE GONE.
//
// `V1_METHODS` (130 rows) and `V2_METHODS` (73 rows) transcribed each WebGL method's name and
// `length` from the Brave 151 capture, for the pre-table install that defined the 203 draw calls
// onto two fetched shell prototypes. Lane L17 moved that data into `webgl/tables.rs`, where every
// one of the 203 is a `method "<name>" : Any len <n> ... = install_webgl_context` row -- and left
// the constants behind, unreferenced, as a SECOND place declaring the same arities. `gl.drawArrays
// .length` is a one-expression tell, so two sources for it is exactly the drift invariant I1 exists
// to prevent; nothing read these, so the tables were already the only truth and this only stops the
// copy being consulted again. Checked before deleting: all 203 names appear in `webgl/tables.rs`
// and not one `length` differed (the 17 with no `len` clause are the `len 0` members).

// ── install: the four tables ──────────────────────────────────────────────────────────────────
//
// Until this lane this file fetched four prototypes out of the generated shell
// registry and defined 203 draw calls and six accessors onto them, and `wire_canvas_oracle` then
// OVERWROTE four of those names with the oracle-backed reads -- two writers on one slot, which is
// exactly what invariant I2 forbids. The rows live in `webgl/tables.rs` now, each member is written
// once, and the installer owns both prototypes.

/// The persona device oracle's `webgl` block, parsed ONCE by `wire_canvas_oracle` and read back
/// here when the four oracle-backed rows are mounted.
///
/// The rows need a value at INSTALL time (each is `build_fn_data` over the recorded device), and an
/// `InstallFn` is handed only `(scope, prototype, row)` -- there is no data channel in a table row,
/// and inventing one for a single family would be the wrong shape. So the op that already parses the
/// oracle leaves the parsed object here. A realm with no oracle (a worker) simply has no entry, and
/// the reads then answer exactly what they answered from the worker's shell stub: nothing.
///
/// The second field says whether the DEVICE's values are in it yet ([`with_device`]): the persona's block is what was
/// recorded, and this PC's GPU answers its own half the first time a page reads it.
pub(crate) struct WebGLOracle(pub v8::Global<v8::Value>, pub bool);

/// The oracle's `webgl` block, or `undefined`. The oracle-backed callbacks read it HERE, per call, and
/// return on `Err`, so `undefined` is the no-device path they have always had. It used to be each
/// function's `data`, which put one realm's object into every function built over it -- a value a V8
/// template cannot hold.
pub(crate) fn oracle_value<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Value> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let g = {
        let st = op_state.borrow();
        st.try_borrow::<WebGLOracle>().map(|o| (o.0.clone(), o.1))
    };
    match g {
        Some((g, true)) => v8::Local::new(scope, &g),
        Some((g, false)) => {
            let recorded = v8::Local::new(scope, &g);
            let v = with_device(scope, recorded);
            op_state.borrow_mut().put(WebGLOracle(v8::Global::new(scope, v), true));
            v
        }
        None => v8::undefined(scope).into(),
    }
}

/// The page made a canvas or asked one for a context: a backend that starts its GPU when a page first
/// needs one starts it now, so the first GL question finds it up -- Chrome's GPU process is up before any page.
pub(crate) fn prepare_gl(scope: &mut v8::PinScope) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let backend = {
        let st = op_state.borrow();
        st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::RenderHandle>()).map(|h| h.0.clone())
    };
    if let Some(b) = backend {
        b.prepare_gl();
    }
}

/// Owner 2026-10-12: the persona's `webgl` block as THIS PC's GPU answers it. Chrome reads every
/// limit, precision format, extension and `WEBGL_debug_renderer_info` string of the device it draws on, so a
/// backend with a device (`RenderBackend::gl_device`) replaces the recording's device values with its own; a PC
/// without a usable GPU has no WebGL at all (Chrome 150 on Linux: no SwiftShader fallback), so the block goes.
/// The browser's own values -- `VENDOR` "WebKit", the version strings, the context attributes -- stay the
/// persona's, and a persona recorded without a device stays without one.
fn with_device<'s>(scope: &mut v8::PinScope<'s, '_>, recorded: v8::Local<'s, v8::Value>) -> v8::Local<'s, v8::Value> {
    let Ok(webgl) = v8::Local::<v8::Object>::try_from(recorded) else { return recorded };
    let (plugged, backend) = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let ctx = st.try_borrow::<spec::kernel::Context>();
        (ctx.is_some(), ctx.and_then(|c| c.get::<spec::RenderHandle>()).map(|h| h.0.clone()))
    };
    // A realm whose plugins left render out (`KORVUS_RENDER=off`) has nothing to draw with: no WebGL, as a PC
    // without a usable GPU, whatever device the persona states.
    if plugged && backend.is_none() {
        return v8::undefined(scope).into();
    }
    // A persona document's GPU (goal 1020 step 3): the device it states is the one a page reads, never this machine's.
    if crate::binding::protos::get_prop(scope, webgl, "_document").is_some_and(|d| !d.is_undefined()) {
        return recorded;
    }
    match backend.map(|b| b.gl_device()) {
        None | Some(spec::GlDevice::Unknown) => recorded,
        Some(spec::GlDevice::Absent) => v8::undefined(scope).into(),
        Some(spec::GlDevice::Present(json)) => {
            let Some(text) = v8::String::new(scope, &json) else { return recorded };
            let Some(device) = v8::json::parse(scope, text).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else {
                return recorded;
            };
            if recorded_is_d3d11(scope, webgl) {
                return with_d3d11_names(scope, webgl, device);
            }
            let Some(keys) = device.get_own_property_names(scope, v8::GetPropertyNamesArgs::default()) else { return recorded };
            for i in 0..keys.length() {
                let Some(k) = keys.get_index(scope, i) else { continue };
                // `_` keys are the device's notes for other paths (`_d3d11Renderer`), not block values
                if k.to_rust_string_lossy(scope).starts_with('_') {
                    continue;
                }
                if let Some(v) = device.get(scope, k) {
                    webgl.set(scope, k, v);
                }
            }
            webgl.into()
        }
    }
}

/// Was the persona's device recorded through ANGLE's D3D11 renderer -- a Windows persona's?
fn recorded_is_d3d11(scope: &mut v8::PinScope, webgl: v8::Local<v8::Object>) -> bool {
    crate::binding::protos::get_prop(scope, webgl, "unmaskedRenderer")
        .filter(|r| r.is_string())
        .is_some_and(|r| r.to_rust_string_lossy(scope).contains("Direct3D11"))
}

/// A Windows persona on a host whose GPU answers through GL (owner 2026-10-17: a Linux host wears a Windows persona):
/// the limits, precision formats, extensions and sample counts stay the recording's -- Windows Chrome's, through
/// ANGLE's D3D11 renderer, which this host has not got -- and `WEBGL_debug_renderer_info`'s two strings name THIS PC's
/// GPU as Windows Chrome names it (the device's `_d3d11Vendor` / `_d3d11Renderer`): the GPU is the machine's own
/// (owner 2026-10-12), never the capture machine's.
fn with_d3d11_names<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    webgl: v8::Local<'s, v8::Object>,
    device: v8::Local<'s, v8::Object>,
) -> v8::Local<'s, v8::Value> {
    use crate::binding::protos::get_prop;
    let (Some(vendor), Some(renderer)) = (get_prop(scope, device, "_d3d11Vendor"), get_prop(scope, device, "_d3d11Renderer")) else {
        return webgl.into();
    };
    for (name, value, pname) in [("unmaskedVendor", vendor, "37445"), ("unmaskedRenderer", renderer, "37446")] {
        if let Some(k) = v8::String::new(scope, name) {
            webgl.set(scope, k.into(), value);
        }
        for params in ["paramsByNum", "paramsByNumV2"] {
            let Some(p) = get_prop(scope, webgl, params).and_then(|p| v8::Local::<v8::Object>::try_from(p).ok()) else { continue };
            if let Some(k) = v8::String::new(scope, pname) {
                if p.has_own_property(scope, k.into()).unwrap_or(false) {
                    p.set(scope, k.into(), value);
                }
            }
        }
    }
    webgl.into()
}

/// Does the persona's recorded device have a GPU at all -- i.e. did the capture record an unmasked
/// WebGL renderer string? `HTMLCanvasElement.getContext` answers `null` for `webgl` / `webgl2`
/// when it did not.
///
/// READ PER CALL, not per install. It used to be a `build_fn_data` boolean baked in by
/// `wire_canvas_oracle`, and that was the whole reason `getContext` could not be a table row:
/// `install_window_element_refs` mounts `HTMLCanvasElement`'s table BEFORE the oracle is parsed, so
/// the value does not exist yet at the row's install moment. It does exist at every moment a page can
/// call the method, which is the only moment that matters -- the oracle is in `OpState` from the last
/// step of the pre-bootstrap block, and no script runs before that. A realm with no oracle (a worker,
/// a bare `JsRuntime`) reads `false`, which is the same answer the absent-device path always gave.
pub(crate) fn oracle_has_webgl(scope: &mut v8::PinScope) -> bool {
    // DIAGNOSTIC, analysis builds only: the persona as a GPU-less Chrome answers it (no WebGL context), to tell
    // whether a verdict follows the WebGL surface
    #[cfg(feature = "analysis")]
    if std::env::var_os("KORVUS_DIAG_NO_WEBGL").is_some_and(|v| v == "1") {
        return false;
    }
    let v = oracle_value(scope);
    let Ok(o) = v8::Local::<v8::Object>::try_from(v) else { return false };
    crate::binding::protos::get_prop(scope, o, "unmaskedRenderer").map(|r| r.is_string()).unwrap_or(false)
}

/// Mount one method row over a callback that takes the ROW'S NAME as its `data` -- the shape all 203
/// recorded draw calls share, since `cb_gl` dispatches on the name. The WebIDL arity guard is the
/// row's own (`live::rows::method_row` does the same for a plain callback); before the cutover the
/// registry-wide arity pass applied it from the same `required` count.
fn gl_method_row(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let data = str_val(scope, member.name);
    let f = build_fn_data(scope, cb_gl, member.name, member.length as i32, data);
    guarded_value(scope, proto, member, f);
}

/// Mount one method row over a callback that reads the recorded DEVICE (the oracle's `webgl` block,
/// [`oracle_value`]) per call, which is what makes `getParameter(37446)` answer the captured renderer
/// string.
fn gl_oracle_row(
    scope: &mut v8::PinScope,
    proto: v8::Local<v8::Object>,
    member: &'static MemberDef,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
) {
    let f = crate::binding::protos::build_fn(scope, cb, member.name, member.length as i32);
    guarded_value(scope, proto, member, f);
}

/// The row's WebIDL arity guard and descriptor, shared by the two mounts above.
fn guarded_value<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    proto: v8::Local<v8::Object>,
    member: &'static MemberDef,
    f: v8::Local<'s, v8::Function>,
) {
    use crate::binding::protos::make_arity_guard;
    let iface = member.label.split("::").next().unwrap_or(member.name);
    let v: v8::Local<v8::Value> = if member.required > 0 {
        make_arity_guard(scope, iface, member.name, member.required as i32, member.length as i32, f.into(), member.ty).into()
    } else {
        f.into()
    };
    crate::install::kits::define_value(scope, proto, member, v, true);
}

/// `gl.canvas` — the element this context was made from, off the private slot `getContext` writes,
/// and `Illegal invocation` for a receiver that carries none. The back-reference used to be an own
/// data property on the context whose only job was to SHADOW the shell's `canvas` stub; the row
/// answers it now, so `Object.getOwnPropertyNames(gl)` is `[]` the way a real context's is.
fn cb_gl_canvas(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match priv_get(scope, args.this(), super::canvas_2d::CANVAS_BACKREF_PRIV) {
        Some(v) => rv.set(v),
        None => crate::binding::protos::illegal_invocation(scope),
    }
}

/// The canvas's `width` / `height` as this context's drawing buffer sees them.
///
/// RECORDED, NOT GUESSED (`oracle/WebGLRenderingContext.json`): a context made on an 8x8 canvas
/// reports 8; resizing the canvas to 37 moves `drawingBufferWidth` to 37, so it is a LIVE read of
/// the element and not a snapshot taken at creation; and a canvas resized to 0 reports **1**,
/// because a zero-sized drawing buffer is clamped to one pixel. Assigning to it is ignored (the row
/// is readonly), which the same file records.
fn drawing_buffer_extent(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, key: &str) -> Option<u32> {
    let canvas = priv_get(scope, this, super::canvas_2d::CANVAS_BACKREF_PRIV)
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    // A LOST context has no drawing buffer: 0, not the clamped 1 (measured).
    if extensions::is_lost(scope, this) {
        return Some(0);
    }
    // `drawingBufferStorage` gave the drawing buffer its own size, kept while the canvas keeps its.
    let size = buffer_size(scope, this);
    if let Some((w, h)) = edit(scope, this, |s| gl2b::drawing_buffer(s, size)) {
        return Some(if key == "width" { w } else { h });
    }
    let n = super::html_canvas_element::canvas_extent(scope, canvas, key).unwrap_or(0.0);
    Some(if n.is_finite() && n >= 1.0 { n as u32 } else { 1 })
}

fn cb_gl_buffer_width(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match drawing_buffer_extent(scope, args.this(), "width") {
        Some(n) => rv.set_uint32(n),
        None => crate::binding::protos::illegal_invocation(scope),
    }
}

fn cb_gl_buffer_height(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    match drawing_buffer_extent(scope, args.this(), "height") {
        Some(n) => rv.set_uint32(n),
        None => crate::binding::protos::illegal_invocation(scope),
    }
}

/// `drawingBufferColorSpace` / `unpackColorSpace`: a `PredefinedColorSpace`, `"srgb"` on a new context.
/// They were the registry's slot kit, whose slot nothing ever wrote, so both read `undefined`.
fn cb_gl_drawing_color_space(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    color_space_get(scope, &args, &mut rv, false);
}
fn cb_gl_unpack_color_space(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    color_space_get(scope, &args, &mut rv, true);
}
fn cb_gl_set_drawing_color_space(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    color_space_set(scope, &args, false);
}
fn cb_gl_set_unpack_color_space(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    color_space_set(scope, &args, true);
}

fn color_space_get(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, rv: &mut v8::ReturnValue, unpack: bool) {
    let this = args.this();
    if priv_get(scope, this, WEBGL_ID_PRIV).is_none() {
        crate::binding::protos::illegal_invocation(scope);
        return;
    }
    let v = edit(scope, this, |s| if unpack { s.params.unpack_color_space } else { s.params.drawing_color_space });
    rv.set(str_val(scope, v));
}

/// The setter of an enumeration attribute: a string outside `PredefinedColorSpace` is ignored
/// (WebIDL), one inside it that this build does not ship throws Blink's TypeError
/// (`ValidateAndConvertColorSpace`).
fn color_space_set(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, unpack: bool) {
    let this = args.this();
    if priv_get(scope, this, WEBGL_ID_PRIV).is_none() {
        crate::binding::protos::illegal_invocation(scope);
        return;
    }
    let Some(s) = args.get(0).to_string(scope) else { return };
    let s = s.to_rust_string_lossy(scope);
    let Some((name, shipped)) = params::predefined_color_space(&s) else { return };
    if !shipped {
        let iface = if edit(scope, this, |s| s.v2) { "WebGL2RenderingContext" } else { "WebGLRenderingContext" };
        let attr = if unpack { "unpackColorSpace" } else { "drawingBufferColorSpace" };
        throw_type_error(
            scope,
            &format!("Failed to set the '{attr}' property on '{iface}': The provided value '{name}' is not a valid enum value of the type PredefinedColorSpace."),
        );
        return;
    }
    edit(scope, this, |s| {
        if unpack {
            s.params.unpack_color_space = name;
        } else {
            s.params.drawing_color_space = name;
        }
    });
}

/// `gl.drawingBufferFormat` — the sized internal format of the drawing buffer, which follows the
/// `alpha` context attribute the page asked for: `RGBA8` (0x8058) with alpha, `RGB8` (0x8051)
/// without. The recorded default context answers 32856 = `RGBA8`
/// (`oracle/WebGLRenderingContext.json`), and the attribute struct is the same one
/// `getContextAttributes()` reports and the backend rasterizes against, so the three cannot
/// disagree.
fn cb_gl_buffer_format(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    const RGBA8: u32 = 0x8058;
    const RGB8: u32 = 0x8051;
    let this = args.this();
    if priv_get(scope, this, WEBGL_ID_PRIV).is_none() {
        crate::binding::protos::illegal_invocation(scope);
        return;
    }
    let alpha = context_attributes(scope, this).alpha;
    rv.set_uint32(if alpha { RGBA8 } else { RGB8 });
}

/// Every `WebGLRenderingContext` / `WebGL2RenderingContext` row with a body.
///
/// The v1 and v2 forms of the four oracle-backed reads are DIFFERENT callbacks (a WebGL2 context has
/// its own parameter and extension tables in the capture), and the row says which it is: the label's
/// interface half. Everything else is version-independent — `cb_gl` records the call into the
/// context's own display list and reads back out of it.
pub fn install_webgl_context(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let v2 = member.label.starts_with("WebGL2");
    match member.name {
        "canvas" => getter_row(scope, proto, member, cb_gl_canvas),
        "drawingBufferWidth" => getter_row(scope, proto, member, cb_gl_buffer_width),
        "drawingBufferHeight" => getter_row(scope, proto, member, cb_gl_buffer_height),
        "drawingBufferFormat" => getter_row(scope, proto, member, cb_gl_buffer_format),
        "drawingBufferColorSpace" => {
            crate::install::live::pair_row(scope, proto, member, cb_gl_drawing_color_space, cb_gl_set_drawing_color_space)
        }
        "unpackColorSpace" => crate::install::live::pair_row(scope, proto, member, cb_gl_unpack_color_space, cb_gl_set_unpack_color_space),
        "getParameter" if v2 => gl_oracle_row(scope, proto, member, super::html_canvas_element::cb_gl2_param),
        "getParameter" => gl_oracle_row(scope, proto, member, super::html_canvas_element::cb_gl_param),
        "getExtension" if v2 => gl_oracle_row(scope, proto, member, super::html_canvas_element::cb_gl2_get_extension),
        "getExtension" => gl_oracle_row(scope, proto, member, super::html_canvas_element::cb_gl_get_extension),
        "getSupportedExtensions" if v2 => gl_oracle_row(scope, proto, member, super::html_canvas_element::cb_gl2_supported_ext),
        "getSupportedExtensions" => gl_oracle_row(scope, proto, member, super::html_canvas_element::cb_gl_supported_ext),
        "getContextAttributes" => gl_oracle_row(scope, proto, member, super::html_canvas_element::cb_gl_ctx_attrs),
        _ => gl_method_row(scope, proto, member),
    }
}

/// The three slot-backed reads of `WebGLActiveInfo` and `WebGLShaderPrecisionFormat`. Both are
/// objects this file MINTS (from `getActiveAttrib` / `getShaderPrecisionFormat`), and both carry
/// their values on v8 privates so the instances have no own properties, which is what a real
/// browser's have.
pub fn install_webgl_info(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let data = str_val(scope, member.name);
    let g = build_fn_data(scope, cb_gl_slot_get, &format!("get {}", member.name), 0, data);
    crate::install::kits::define_accessor(scope, proto, member, g, None);
}
