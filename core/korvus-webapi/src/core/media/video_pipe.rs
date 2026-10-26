//! `core/media/video_pipe.rs` -- a `<video>`'s pictures: its coded frames decoded on a media thread of its own, and
//! the picture for the playback position handed to the painter.
//!
//! The element's clock (`media_load.rs`: a position, and the instant it started playing from it) is shared with the
//! thread, which decodes from the keyframe before the position, keeps a few frames ahead while the element plays,
//! wakes when the next one is due, and shows it. The painter turns the picture shown into RGBA, once per picture --
//! it never waits for a decode, and a picture no one paints is never converted.
//!
//! A seek (the position moved back, or past a later keyframe) starts the decoder over from the keyframe before the
//! new position; a change to frames already passed (an MSE `remove`, an append over buffered time) does too. A new
//! configuration record -- an adaptive stream's next rendition, with its own SPS -- gets a decoder of its own at its
//! first keyframe.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::core::media::demux::Bytes;

/// One coded video frame.
#[derive(Clone)]
pub struct Coded {
    pub dts: f64,
    pub pts: f64,
    pub duration: f64,
    pub key: bool,
    pub data: Bytes,
    pub codec: Arc<spec::VideoCodec>,
}

/// The element's playback clock: the position, and while it plays the instant it left that position.
#[derive(Clone, Copy, Debug, Default)]
pub struct Clock {
    pub position: f64,
    pub since: Option<Instant>,
}

impl Clock {
    fn now(&self) -> f64 {
        self.position + self.since.map_or(0.0, |s| s.elapsed().as_secs_f64())
    }
}

struct State {
    /// Coded frames in decode order.
    samples: Vec<Coded>,
    /// Bumped when frames change anywhere but at the end: the thread starts over from a keyframe.
    epoch: u64,
    clock: Clock,
    /// The picture for the position: its presentation time, its planes, and their RGBA once a painter asked.
    shown: Option<Shown>,
    dead: bool,
}

struct Shown {
    pts: f64,
    yuv: Arc<spec::YuvFrame>,
    rgba: Option<Arc<spec::Rgba8>>,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    /// The frames are a file's (`<video src>`), not a `MediaSource`'s: a picture whose stream states no colour matrix
    /// is BT.601 below 720 lines, as Chromium's FFmpeg path takes one (`AVStreamToVideoDecoderConfig`); an MSE stream's
    /// is BT.709 (`YuvFrame::to_rgba`). Chrome 150 draws WPT's `2x2-green.mp4` as `1, 128, 2` from a file and the MSE
    /// clip `test-v-128k-320x240` red as `255, 24, 0` -- one matrix each, neither stream naming one.
    file: bool,
}

/// A `<video>`'s decoding thread and what it shows. Dropped with the element's media state: the thread ends.
pub struct Pipe {
    shared: Arc<Shared>,
}

/// How far ahead of the position a playing element's frames are decoded.
const AHEAD: f64 = 0.25;
/// At most this many decoded frames wait to be shown.
const QUEUE: usize = 8;

impl Pipe {
    /// A pipe whose decoders `backend` makes, for a file's frames (`file`) or a `MediaSource`'s.
    pub fn new(backend: Arc<dyn spec::RenderBackend>, file: bool) -> Pipe {
        let shared = Arc::new(Shared {
            state: Mutex::new(State { samples: Vec::new(), epoch: 0, clock: Clock::default(), shown: None, dead: false }),
            wake: Condvar::new(),
            file,
        });
        let thread_shared = shared.clone();
        let _ = std::thread::Builder::new().name("korvus-video".into()).spawn(move || run(thread_shared, backend));
        Pipe { shared }
    }

    fn with<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        let mut st = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let r = f(&mut st);
        drop(st);
        self.shared.wake.notify_all();
        r
    }

    /// Add coded frames. Frames that follow every frame held are appended; any others replace what they overlap
    /// (and the frames that depended on it, up to the next keyframe) and the decoder starts over.
    pub fn add(&self, mut new: Vec<Coded>) {
        if new.is_empty() {
            return;
        }
        new.sort_by(|a, b| a.dts.total_cmp(&b.dts));
        self.with(|st| {
            let last = st.samples.last().map_or(f64::NEG_INFINITY, |s| s.dts);
            if new[0].dts > last {
                st.samples.extend(new);
                return;
            }
            let (lo, hi) = new.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), s| (lo.min(s.pts), hi.max(s.pts + s.duration)));
            remove_range(&mut st.samples, lo, hi);
            st.samples.extend(new);
            st.samples.sort_by(|a, b| a.dts.total_cmp(&b.dts));
            st.epoch += 1;
        });
    }

    /// Remove the frames presented in `[start, end)` and those that depend on them (MSE's coded frame removal).
    pub fn remove(&self, start: f64, end: f64) {
        self.with(|st| {
            let before = st.samples.len();
            remove_range(&mut st.samples, start, end);
            if st.samples.len() != before {
                st.epoch += 1;
            }
        });
    }

    /// The element's clock changed. A position that does not continue the old one (a seek, a loop back to the start,
    /// a new load) starts the decoder over.
    pub fn set_clock(&self, clock: Clock) {
        self.with(|st| {
            if (clock.now() - st.clock.now()).abs() > 0.05 {
                st.epoch += 1;
            }
            st.clock = clock;
        });
    }

    /// The picture for the position, as last decoded -- turned into RGBA here, once per picture, on the painter's
    /// thread: the media thread only decodes, and a picture no one paints costs no conversion.
    pub fn frame(&self) -> Option<Arc<spec::Rgba8>> {
        let (pts, yuv) = {
            let st = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            let shown = st.shown.as_ref()?;
            if let Some(rgba) = &shown.rgba {
                return Some(rgba.clone());
            }
            (shown.pts, shown.yuv.clone())
        };
        let rgba = Arc::new(yuv.to_rgba());
        let mut st = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(shown) = st.shown.as_mut().filter(|s| s.pts == pts && Arc::ptr_eq(&s.yuv, &yuv)) {
            shown.rgba = Some(rgba.clone());
        }
        Some(rgba)
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        self.with(|st| st.dead = true);
    }
}

fn remove_range(samples: &mut Vec<Coded>, start: f64, end: f64) {
    // a removed frame takes the frames after it in decode order with it, up to the next keyframe
    let mut dropping = false;
    samples.retain(|s| {
        let inside = s.pts >= start && s.pts < end;
        if inside {
            dropping = true;
            return false;
        }
        if s.key {
            dropping = false;
        }
        !dropping
    });
}

/// The thread's decoding position.
struct Decoding {
    decoder: Option<(Arc<spec::VideoCodec>, Box<dyn spec::VideoDecoder>)>,
    /// The next frame to feed, by index into the samples.
    next: usize,
    /// The presentation times fed and not yet out, smallest first: each picture out takes the smallest (a decoder
    /// gives its pictures in presentation order).
    pending: Vec<f64>,
    queue: VecDeque<(f64, spec::YuvFrame)>,
    epoch: u64,
    /// The decoder was drained at the end of the frames: the next frames start at a keyframe.
    drained: bool,
    /// Decoding just started over: the first picture out is shown whatever its time (a buffer that starts a little
    /// after the position still shows its first frame).
    fresh: bool,
}

impl Decoding {
    fn restart(&mut self, at: usize) {
        self.decoder = None;
        self.next = at;
        self.pending.clear();
        self.queue.clear();
        self.drained = false;
        self.fresh = true;
    }

    fn take(&mut self, frames: Vec<spec::YuvFrame>) {
        for f in frames {
            if self.pending.is_empty() {
                continue;
            }
            let pts = self.pending.remove(0);
            let at = self.queue.iter().position(|(p, _)| *p > pts).unwrap_or(self.queue.len());
            self.queue.insert(at, (pts, f));
        }
        // a decoder that dropped a frame leaves its time behind: forget times far older than the newest out
        if self.pending.len() > 32 {
            self.pending.drain(..self.pending.len() - 32);
        }
    }
}

/// The keyframe to start decoding from for position `t`: the last one (in decode order) presented at or before `t`,
/// else the first.
fn keyframe_for(samples: &[Coded], t: f64) -> usize {
    let mut best = None;
    for (i, s) in samples.iter().enumerate() {
        if s.key {
            if s.pts <= t + 1e-6 || best.is_none() {
                best = Some(i);
            } else {
                break;
            }
        }
    }
    best.unwrap_or(0)
}

fn run(shared: Arc<Shared>, backend: Arc<dyn spec::RenderBackend>) {
    let mut d = Decoding { decoder: None, next: 0, pending: Vec::new(), queue: VecDeque::new(), epoch: u64::MAX, drained: false, fresh: true };
    loop {
        let st = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if st.dead {
            return;
        }
        let t = st.clock.now();
        let playing = st.clock.since.is_some();
        // frames changed under the decoder, the position jumped, or frames came after the decoder was drained: start
        // over from the keyframe before the position
        if st.epoch != d.epoch || (d.drained && d.next < st.samples.len()) {
            d.epoch = st.epoch;
            let k = keyframe_for(&st.samples, t);
            d.restart(k);
        }
        // the picture due: the newest decoded one presented at or before the position, once no frame still to come can
        // be presented before it (a later picture is out, or nothing fed or about to be fed is presented by then) --
        // and right after a start with nothing that early, the first one out
        let mut cand = None;
        while let Some((p, _)) = d.queue.front() {
            if *p > t + 1e-6 {
                break;
            }
            cand = d.queue.pop_front();
        }
        let upcoming = st.samples[d.next.min(st.samples.len())..].iter().take(16).map(|s| s.pts).fold(f64::INFINITY, f64::min);
        let settled = !d.queue.is_empty() || (upcoming > t + 1e-6 && d.pending.first().is_none_or(|p| *p > t + 1e-6));
        // a playing element shows the newest it has at once: a decoder behind the clock shows what it can
        let mut due = None;
        if let Some(c) = cand {
            if settled || playing {
                due = Some(c);
            } else {
                d.queue.push_front(c);
            }
        } else if d.fresh && !d.queue.is_empty() {
            due = d.queue.pop_front();
        }
        // a decoder that fell more than a second behind a playing clock jumps to the keyframe before it, dropping the
        // frames between, as a slow machine's player drops them
        if playing && due.is_none() && d.queue.is_empty() {
            let k = keyframe_for(&st.samples, t);
            let behind = st.samples.get(d.next).is_some_and(|s| s.pts < t - 1.0);
            if behind && k > d.next {
                d.restart(k);
                d.fresh = false;
            }
        }
        // more frames are wanted while nothing is due for the position, or a playing element runs short ahead of it
        let horizon = if playing { t + AHEAD } else { t };
        let decoded_to = d.queue.back().map(|(p, _)| *p).or(due.as_ref().map(|(p, _)| *p)).unwrap_or(f64::NEG_INFINITY);
        let want_more = d.next < st.samples.len() && d.queue.len() < QUEUE && (decoded_to < horizon || d.fresh && due.is_none() && d.queue.is_empty());
        let feed = want_more.then(|| st.samples[d.next].clone());
        // at the end of the frames, a decoder holding pictures back gives them up
        let drain = !want_more && d.next >= st.samples.len() && !d.pending.is_empty() && decoded_to < horizon && !d.drained;
        let next_due = d.queue.front().map(|(p, _)| *p);
        if let Some((pts, mut frame)) = due {
            drop(st);
            d.fresh = false;
            if shared.file && frame.matrix == 2 && frame.height < 720 {
                frame.matrix = 6;
            }
            let mut st = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            if st.epoch == d.epoch {
                st.shown = Some(Shown { pts, yuv: Arc::new(frame), rgba: None });
            }
            continue;
        }
        drop(st);
        if let Some(s) = feed {
            d.next += 1;
            let same = d.decoder.as_ref().is_some_and(|(c, _)| Arc::ptr_eq(c, &s.codec) || **c == *s.codec);
            if !same {
                // a new configuration starts at a keyframe, with a decoder of its own
                if !s.key {
                    continue;
                }
                d.decoder = backend.video_decoder(&s.codec).map(|dec| (s.codec.clone(), dec));
                d.pending.clear();
            }
            if let Some((_, dec)) = d.decoder.as_mut() {
                let at = d.pending.iter().position(|p| *p > s.pts).unwrap_or(d.pending.len());
                d.pending.insert(at, s.pts);
                let frames = dec.decode(s.data.as_slice());
                d.take(frames);
            }
            continue;
        }
        if drain {
            if let Some((_, dec)) = d.decoder.as_mut() {
                let frames = dec.flush();
                d.take(frames);
            }
            d.pending.clear();
            d.drained = true;
            continue;
        }
        // nothing to do until the next picture is due, or something changes
        let st = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if st.dead {
            return;
        }
        let wait = match (st.clock.since.is_some(), next_due) {
            (true, Some(p)) => Duration::from_secs_f64((p - st.clock.now()).clamp(0.001, 1.0)),
            (true, None) => Duration::from_millis(50),
            (false, _) => Duration::from_millis(500),
        };
        let _ = shared.wake.wait_timeout(st, wait);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A backend whose decoder gives each sample's first byte back as a 1x1 grey picture, holding one picture back
    /// as a reordering decoder does.
    struct Fake;
    struct FakeDecoder(Option<u8>);
    impl spec::RenderBackend for Fake {
        fn name(&self) -> &str {
            "fake"
        }
        fn video_decoder(&self, _codec: &spec::VideoCodec) -> Option<Box<dyn spec::VideoDecoder>> {
            Some(Box::new(FakeDecoder(None)))
        }
    }
    fn grey(v: u8) -> spec::YuvFrame {
        spec::YuvFrame { width: 1, height: 1, y: vec![v], u: vec![128], v: vec![128], matrix: 1, full_range: true }
    }
    impl spec::VideoDecoder for FakeDecoder {
        fn decode(&mut self, sample: &[u8]) -> Vec<spec::YuvFrame> {
            self.0.replace(sample[0]).map(grey).into_iter().collect()
        }
        fn flush(&mut self) -> Vec<spec::YuvFrame> {
            self.0.take().map(grey).into_iter().collect()
        }
    }

    /// Ten frames at 10 fps, a keyframe every five; frame `i` is drawn as grey level `i * 10`.
    fn frames() -> Vec<Coded> {
        let codec = Arc::new(spec::VideoCodec { fourcc: *b"test", config: Vec::new() });
        (0..10u8)
            .map(|i| Coded {
                dts: i as f64 / 10.0,
                pts: i as f64 / 10.0,
                duration: 0.1,
                key: i % 5 == 0,
                data: Bytes::whole(Arc::new(vec![i * 10])),
                codec: codec.clone(),
            })
            .collect()
    }

    fn shown(p: &Pipe, want: u8) -> bool {
        for _ in 0..200 {
            if p.frame().is_some_and(|f| f.data[0] == want) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    #[test]
    fn a_paused_element_shows_the_frame_at_its_position() {
        let p = Pipe::new(Arc::new(Fake), true);
        p.add(frames());
        assert!(shown(&p, 0), "the first frame at 0");
        p.set_clock(Clock { position: 0.72, since: None });
        assert!(shown(&p, 70), "a seek forward shows frame 7");
        p.set_clock(Clock { position: 0.25, since: None });
        assert!(shown(&p, 20), "a seek back shows frame 2");
        p.set_clock(Clock { position: 0.95, since: None });
        assert!(shown(&p, 90), "the last frame comes out of the decoder at the end");
    }

    #[test]
    fn a_playing_element_moves_through_its_frames() {
        let p = Pipe::new(Arc::new(Fake), true);
        p.add(frames());
        p.set_clock(Clock { position: 0.0, since: Some(Instant::now()) });
        assert!(shown(&p, 30), "frame 3 by 0.3 s");
        assert!(shown(&p, 60), "frame 6 by 0.6 s");
    }

    #[test]
    fn removed_frames_are_not_shown() {
        let p = Pipe::new(Arc::new(Fake), true);
        p.add(frames());
        p.remove(0.5, 1.0);
        p.set_clock(Clock { position: 0.8, since: None });
        assert!(shown(&p, 40), "the newest frame left before 0.8 is frame 4");
    }
}
