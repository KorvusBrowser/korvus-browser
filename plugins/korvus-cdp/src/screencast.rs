//! `Page.startScreencast` -- the page's frames to a DevTools client, as Chrome's screencast sends them (D74 step 3).
//!
//! A front end (D72) shows the page from these: `Page.screencastFrame` carries one frame, JPEG or PNG and base64, with
//! its metadata, and waits for `Page.screencastFrameAck` -- at most [`MAX_IN_FLIGHT`] go unacknowledged, Chrome's
//! `kMaxScreencastFramesInFlight`, so a client that draws slowly is sent fewer frames rather than a queue of stale
//! ones. The frames are the frame tap's (`webapi::deliver_frame`): a changed picture, at most one per display refresh,
//! right after the page's animation-frame callbacks.
//!
//! Three places, so the page's thread pays for a copy and nothing more: the SINK runs on the page's thread as each
//! frame is painted and hands a copy on; an ENCODER thread lays it on white (a page's own background is transparent
//! where Chrome's base background is white), scales it to `maxWidth`/`maxHeight` and encodes it; a WRITER task puts
//! the event on the connection's write half (`fetch::Out`), which the page loop shares.
//!
//! Unlike Chrome, the newest frame painted while the client is behind is KEPT and sent at its next ack: Chrome drops
//! it, and a page that stops changing then never shows the client its last picture.

use std::sync::{mpsc, Arc, Mutex};

use base64::Engine as _;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

use crate::fetch::Out;

/// Frames sent and not yet acknowledged: Chrome's `kMaxScreencastFramesInFlight`.
const MAX_IN_FLIGHT: u32 = 2;

/// Chrome's `kDefaultScreenshotQuality`.
const DEFAULT_QUALITY: u8 = 80;

/// What a `Page.startScreencast` asked for.
#[derive(Clone, Copy)]
struct Params {
    jpeg: bool,
    quality: u8,
    max_w: Option<u32>,
    max_h: Option<u32>,
    every_nth: u64,
}

/// A painted frame on its way to the encoder.
struct Job {
    id: u64,
    w: u32,
    h: u32,
    rgba: Vec<u8>,
    scroll: (f64, f64),
}

/// The running screencast of this page's process (one page per process).
struct Cast {
    params: Params,
    in_flight: u32,
    /// Frames painted since the start, for `everyNthFrame`.
    seen: u64,
    /// The next frame's `sessionId` (Chrome numbers them from 1).
    next_id: u64,
    /// The newest frame painted while `MAX_IN_FLIGHT` were unacknowledged.
    pending: Option<Job>,
    to_encoder: mpsc::Sender<Job>,
}

static CAST: Mutex<Option<Cast>> = Mutex::new(None);

fn cast() -> std::sync::MutexGuard<'static, Option<Cast>> {
    CAST.lock().unwrap_or_else(|e| e.into_inner())
}

/// `Page.startScreencast`: start (or restart) sending this page's frames on `out`, stamped with `session`.
pub(crate) fn start(engine: &mut engine::Engine, params: &Value, out: Out, session: Option<String>) -> Result<Value, String> {
    let backend = engine.render_backend().ok_or_else(|| "Unable to start screencast".to_string())?;
    let num = |k: &str| params.get(k).and_then(Value::as_i64);
    // Chrome's defaults and clamps (`PageHandler::StartScreencast`): PNG unless `jpeg`, quality 80 unless in 0..=100,
    // every frame unless a positive `everyNthFrame`.
    let p = Params {
        jpeg: params.get("format").and_then(Value::as_str) == Some("jpeg"),
        quality: num("quality").filter(|q| (0..=100).contains(q)).map_or(DEFAULT_QUALITY, |q| q as u8),
        max_w: num("maxWidth").filter(|&v| v > 0).map(|v| v as u32),
        max_h: num("maxHeight").filter(|&v| v > 0).map(|v| v as u32),
        every_nth: num("everyNthFrame").filter(|&v| v > 0).map_or(1, |v| v as u64),
    };
    let (to_encoder, jobs) = mpsc::channel::<Job>();
    let (to_writer, mut lines) = tokio::sync::mpsc::unbounded_channel::<String>();
    std::thread::Builder::new()
        .name("korvus-screencast".into())
        .spawn(move || {
            while let Ok(job) = jobs.recv() {
                if let Some(ev) = encode(&*backend, p, job, session.as_deref()) {
                    if to_writer.send(ev).is_err() {
                        return;
                    }
                }
            }
        })
        .map_err(|e| e.to_string())?;
    let mut out = out;
    tokio::spawn(async move {
        while let Some(line) = lines.recv().await {
            if out.send(Message::Text(line)).await.is_err() {
                return;
            }
        }
    });
    // Replacing a running cast drops its encoder's channel, which ends its thread and its writer.
    *cast() = Some(Cast { params: p, in_flight: 0, seen: 0, next_id: 1, pending: None, to_encoder });
    plugin_host::set_frame_sink(Some(Arc::new(on_frame)));
    // The picture already on screen is the first frame, as Chrome's screencast starts with one.
    engine.offer_frame_again();
    Ok(json!({}))
}

/// `Page.stopScreencast`.
pub(crate) fn stop() {
    plugin_host::set_frame_sink(None);
    *cast() = None;
}

/// `Page.screencastFrameAck`: one frame fewer in flight, and the newest held one sent in its place.
pub(crate) fn ack() {
    let mut g = cast();
    let Some(c) = g.as_mut() else { return };
    c.in_flight = c.in_flight.saturating_sub(1);
    if let Some(mut job) = c.pending.take() {
        job.id = c.next_id;
        c.next_id += 1;
        c.in_flight += 1;
        let _ = c.to_encoder.send(job);
    }
}

/// The sink, on the page's thread as each frame is painted.
fn on_frame(f: &plugin_host::SinkFrame) {
    let mut g = cast();
    let Some(c) = g.as_mut() else { return };
    c.seen += 1;
    if (c.seen - 1) % c.params.every_nth != 0 {
        return;
    }
    let job = Job { id: 0, w: f.width, h: f.height, rgba: f.rgba.to_vec(), scroll: f.scroll };
    if c.in_flight >= MAX_IN_FLIGHT {
        c.pending = Some(job);
        return;
    }
    let job = Job { id: c.next_id, ..job };
    c.next_id += 1;
    c.in_flight += 1;
    let _ = c.to_encoder.send(job);
}

/// One `Page.screencastFrame` event, as a line for the socket.
fn encode(backend: &dyn spec::RenderBackend, p: Params, job: Job, session: Option<&str>) -> Option<String> {
    let mut rgba = job.rgba;
    on_white(&mut rgba);
    let (w, h) = (job.w, job.h);
    let scale = [p.max_w.map(|m| m as f64 / w as f64), p.max_h.map(|m| m as f64 / h as f64)]
        .into_iter()
        .flatten()
        .fold(1.0f64, f64::min);
    let (ow, oh) = (((w as f64 * scale).round() as u32).max(1), ((h as f64 * scale).round() as u32).max(1));
    let buf = if (ow, oh) == (w, h) { spec::Rgba8 { width: w, height: h, data: rgba } } else { downscale(&rgba, w, h, ow, oh) };
    let bytes = if p.jpeg {
        backend.encode_image(&buf, spec::ImageFormat::Jpeg, Some(p.quality as f64 / 100.0))?
    } else {
        backend.encode_image(&buf, spec::ImageFormat::Png, None)?
    };
    let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    let mut ev = json!({
        "method": "Page.screencastFrame",
        "params": {
            "data": base64::engine::general_purpose::STANDARD.encode(bytes),
            "metadata": {
                "offsetTop": 0,
                "pageScaleFactor": 1,
                "deviceWidth": w,
                "deviceHeight": h,
                "scrollOffsetX": job.scroll.0,
                "scrollOffsetY": job.scroll.1,
                "timestamp": timestamp,
            },
            "sessionId": job.id,
        },
    });
    if let Some(s) = session {
        ev["sessionId"] = json!(s);
    }
    Some(ev.to_string())
}

/// Lay unpremultiplied RGBA on opaque white: what is transparent in the page's own paint is the base background,
/// white in Chrome.
fn on_white(rgba: &mut [u8]) {
    for px in rgba.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a == 255 {
            continue;
        }
        for c in &mut px[..3] {
            *c = ((*c as u32 * a + 255 * (255 - a) + 127) / 255) as u8;
        }
        px[3] = 255;
    }
}

/// Area-average `src` (`w` x `h`) down to `ow` x `oh`: every source pixel counts by how much of it a target pixel covers.
fn downscale(src: &[u8], w: u32, h: u32, ow: u32, oh: u32) -> spec::Rgba8 {
    let (sx, sy) = (w as f64 / ow as f64, h as f64 / oh as f64);
    let mut out = vec![0u8; (ow * oh * 4) as usize];
    for oy in 0..oh {
        let (y0, y1) = (oy as f64 * sy, (oy + 1) as f64 * sy);
        for ox in 0..ow {
            let (x0, x1) = (ox as f64 * sx, (ox + 1) as f64 * sx);
            let mut acc = [0f64; 4];
            let mut area = 0f64;
            let mut y = y0.floor() as u32;
            while (y as f64) < y1 && y < h {
                let wy = (y1.min(y as f64 + 1.0) - y0.max(y as f64)).max(0.0);
                let mut x = x0.floor() as u32;
                while (x as f64) < x1 && x < w {
                    let wx = (x1.min(x as f64 + 1.0) - x0.max(x as f64)).max(0.0);
                    let i = ((y * w + x) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += src[i + c] as f64 * wx * wy;
                    }
                    area += wx * wy;
                    x += 1;
                }
                y += 1;
            }
            let o = ((oy * ow + ox) * 4) as usize;
            for c in 0..4 {
                out[o + c] = if area > 0.0 { (acc[c] / area).round() as u8 } else { 0 };
            }
        }
    }
    spec::Rgba8 { width: ow, height: oh, data: out }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transparent_paint_lies_on_white_and_opaque_paint_is_kept() {
        let mut px = vec![0, 0, 0, 0, 10, 20, 30, 255, 0, 0, 0, 128];
        on_white(&mut px);
        assert_eq!(&px[0..4], &[255, 255, 255, 255]);
        assert_eq!(&px[4..8], &[10, 20, 30, 255]);
        assert_eq!(&px[8..12], &[127, 127, 127, 255]);
    }

    #[test]
    fn a_frame_is_averaged_down_by_area() {
        // 2x2: black, white / white, black -> one grey pixel
        let src = [0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 255];
        let out = downscale(&src, 2, 2, 1, 1);
        assert_eq!((out.width, out.height), (1, 1));
        assert_eq!(&out.data, &[128, 128, 128, 255]);
    }
}
