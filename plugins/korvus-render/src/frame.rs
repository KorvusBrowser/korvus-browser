//! C1 (goal 1021, plugin ABI 3 wave 1 -- the D29 streaming core): the frame tap.
//!
//! The document compositor paints the page to an offscreen RGBA8 buffer (korvus-webapi's
//! `render_document_rgba`, rastered through this crate's Skia backend via `RenderHandle::raster_2d`).
//! When a loaded plugin WANTS frames (`plugin_host::has_frames()` -- any plugin with `want_frames` set
//! and an `on_frame`), the painted buffer is published here and the runtime drains it to
//! `plugin_host::on_frame`, which hands a `KorvusFrame` (top-left origin) to every frame-wanting plugin
//! in load order.
//!
//! NON-replaceable (invariant J1): this only READS what the renderer drew -- it keeps a copy of the last
//! painted buffer and never changes a pixel. The renderer itself (SkiaBackend, NullBackend) is untouched
//! and stays the one, unswappable pixel engine.
//!
//! ZERO-COST when no plugin wants frames: [`wanted`] is a single relaxed atomic load, and [`publish`]
//! returns before it copies anything unless the gate is armed. The runtime arms the gate once, from
//! `plugin_host::has_frames()`, as it sets up a page process (and disarms it if the loaded set loses its
//! last frame-wanting plugin). A page with no frame-wanting plugin pays one atomic read per paint and
//! nothing more -- the same shape the readback slot gates on (`plugin_host::has_readback`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{OnceLock, RwLock};

/// One painted page frame: top-left-origin unpremultiplied RGBA8 (`width * height * 4` bytes, the layout
/// the compositor rasters and `getImageData` exposes) and `t_us`, the frame's timestamp in microseconds
/// from the page's start. The shape `plugin_host::on_frame` / the header's `KorvusFrame` want.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub t_us: f64,
}

/// The gate: a plugin wants frames. A single relaxed load on the hot paint path ([`publish`]'s first
/// act), so an unwatched page costs one atomic read per paint and never a buffer copy.
static WANTED: AtomicBool = AtomicBool::new(false);

fn slot() -> &'static RwLock<Option<Frame>> {
    static S: OnceLock<RwLock<Option<Frame>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// Arm or disarm the frame tap. The runtime calls this once from `plugin_host::has_frames()` as a page
/// process starts, and again if the loaded set changes: `true` makes [`publish`] keep the latest frame,
/// `false` drops any held frame so a disarmed tap keeps nothing.
pub fn set_wanted(on: bool) {
    WANTED.store(on, Ordering::Relaxed);
    if !on {
        *slot().write().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// Whether a plugin wants frames -- the producer's gate, checked before it builds a frame to publish.
pub fn wanted() -> bool {
    WANTED.load(Ordering::Relaxed)
}

/// Publish the page's freshly-painted frame (top-left-origin RGBA8). A no-op -- before any copy -- when
/// no plugin wants frames. The owning paint path (korvus-webapi's compositor walk, driven at the
/// runtime's rendering opportunity) calls this with the buffer it just rastered; the last frame
/// published wins until it is taken.
// TODO(1021-verify): the producer lives in korvus-webapi (`render_document_rgba`, which has the Rgba8)
// and the driver in korvus-runtime (`event_loop::rendering_opportunity` -> `render_document_frame`).
// Neither is this crate's to edit (wave-1 owns them separately); this is the render-side store they
// publish into. webapi reaches `plugin_host::on_frame` directly (it depends on plugin_host, not on
// render), so the integration pass decides whether frames flow webapi -> plugin_host::on_frame directly
// or webapi/runtime -> this tap -> plugin_host::on_frame. Either way the gate/API here is the render
// contract: arm from has_frames(), publish the painted buffer, drain with take_latest().
pub fn publish(width: u32, height: u32, rgba: &[u8], t_us: f64) {
    if !wanted() {
        return;
    }
    let frame = Frame { width, height, rgba: rgba.to_vec(), t_us };
    *slot().write().unwrap_or_else(|e| e.into_inner()) = Some(frame);
}

/// Publish a freshly-painted [`spec::Rgba8`] frame -- the shape the compositor walk produces -- at
/// `t_us`. A thin wrapper over [`publish`] so a caller that already holds an `Rgba8` need not spell out
/// its fields; the same gate applies (a no-op, with no copy, when no plugin wants frames).
pub fn publish_rgba(buf: &spec::Rgba8, t_us: f64) {
    if !wanted() {
        return;
    }
    publish(buf.width, buf.height, &buf.data, t_us);
}

/// The latest published frame, cloned, without clearing it -- for a consumer that polls.
pub fn latest() -> Option<Frame> {
    slot().read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Take the latest published frame, leaving the tap empty. The runtime drains it at each rendering
/// opportunity and hands it to `plugin_host::on_frame`.
pub fn take_latest() -> Option<Frame> {
    slot().write().unwrap_or_else(|e| e.into_inner()).take()
}

/// The latest published frame encoded as a PNG the way Chrome 150 encodes it (the same encoder
/// `toDataURL` / `korvus fetch --screenshot` use, [`crate::encode_png`]), or `None` when nothing has
/// been published. A convenience for the C6 screenshot seam when the caller wants the LAST PAINTED
/// frame rather than a fresh render.
pub fn latest_png() -> Option<Vec<u8>> {
    let f = latest()?;
    crate::encode_png(&spec::Rgba8 { width: f.width, height: f.height, data: f.rgba })
}
