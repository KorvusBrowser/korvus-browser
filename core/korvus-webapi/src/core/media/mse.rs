//! `core/media/mse.rs` -- Media Source Extensions: the `SourceBuffer`s a `MediaSource` holds, the bytes appended to
//! them, the time they buffer, and the media element a source is attached to.
//!
//! WHAT WAS HERE BEFORE: nothing past the type check. A `MediaSource` stayed `closed` because no element ever attached
//! one, so `sourceopen` never fired and every MSE player -- Vimeo's, YouTube's, hls.js, dash.js -- sat on its first
//! step: minea.com's background videos and every streaming site's player were black.
//!
//! THE MODEL is the spec's, in Blink's order. Attaching (a media element loading the source's object URL) opens the
//! source and queues `sourceopen`. `appendBuffer` sets `updating`, queues `updatestart`, then -- a task later -- runs
//! the segment parser loop over the bytes (`demux.rs`), the coded frame processing (`timestampOffset`, `mode`, the
//! append window) and the buffered ranges, and fires `update` and `updateend`. The first initialization segment of
//! every buffer takes the element to HAVE_METADATA; the frames buffered at the playback position take it further, and
//! a playing element that runs out of them waits (`media_load.rs`). Video frames go on to the element's decoder
//! (`video_pipe.rs`); an audio frame's place in time is all that is kept of it.
//!
//! A track's buffered ranges are its frames' presentation intervals, joined across a gap shorter than twice the
//! longest frame (Chromium's `SourceBufferRange` fudge room); a buffer's are the intersection of its tracks', the
//! element's the intersection of the active buffers' -- each track's last range reaching the highest end once the
//! stream has ended.

use std::collections::HashMap;
use std::sync::Arc;

use deno_core::v8;

use crate::binding::instance_state as st;
use crate::binding::protos::{build_fn, build_fn_data, make_dom_exc, str_val, throw_type_error};
use crate::core::media::demux;
use crate::install::kits::define_accessor;
use crate::install::live::method_row;
use crate::install::MemberDef;

pub(crate) const SB: &str = "SourceBuffer";

const REMOVED: &str = "This SourceBuffer has been removed from the parent media source.";
const STILL_UPDATING: &str = "This SourceBuffer is still processing an 'appendBuffer' or 'remove' operation.";
const NOT_OPEN: &str = "The MediaSource's readyState is not 'open'.";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ReadyState {
    Closed,
    Open,
    Ended,
}

impl ReadyState {
    pub(crate) fn name(self) -> &'static str {
        match self {
            ReadyState::Closed => "closed",
            ReadyState::Open => "open",
            ReadyState::Ended => "ended",
        }
    }
}

struct Source {
    obj: v8::Global<v8::Object>,
    state: ReadyState,
    duration: f64,
    element: Option<u32>,
    buffers: Vec<u32>,
    active: Vec<u32>,
    list: v8::Global<v8::Object>,
    active_list: v8::Global<v8::Object>,
}

struct TrackBuf {
    kind: demux::Kind,
    /// The coded frames buffered, by presentation time: (start, end, keyframe).
    frames: Vec<(f64, f64, bool)>,
    /// The longest frame seen: frames closer than twice it are one range.
    max_frame: f64,
    /// The newest configuration record (video): the frames appended after it decode with it.
    codec: Option<Arc<spec::VideoCodec>>,
}

struct Buffer {
    obj: v8::Global<v8::Object>,
    /// `None` once removed from its source.
    source: Option<u32>,
    parser: demux::Parser,
    updating: bool,
    /// Bumped by `abort()`: the append it cut short does nothing when its task runs.
    op: u64,
    sequence: bool,
    offset: f64,
    window: (f64, f64),
    tracks: HashMap<u64, TrackBuf>,
    first_init: bool,
    /// `sequence` mode: where the next coded frame group starts.
    group_end: Option<f64>,
}

enum Target {
    Source(u32),
    Buffer(u32),
    List(u32, bool),
}

enum Job {
    Fire(Target, &'static str),
    Append { buffer: u32, op: u64, bytes: Vec<u8> },
    Remove { buffer: u32, op: u64, start: f64, end: f64 },
}

#[derive(Default)]
pub(crate) struct Mse {
    sources: HashMap<u32, Source>,
    buffers: HashMap<u32, Buffer>,
    next: u32,
    jobs: HashMap<u64, Job>,
    next_job: u64,
}

fn with_mse<R>(scope: &mut v8::PinScope, f: impl FnOnce(&mut Mse) -> R) -> R {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut s = op_state.borrow_mut();
    if s.try_borrow::<Mse>().is_none() {
        s.put(Mse::default());
    }
    f(s.borrow_mut::<Mse>())
}

fn queue(scope: &mut v8::PinScope, job: Job) {
    let seq = with_mse(scope, |m| {
        m.next_job += 1;
        m.jobs.insert(m.next_job, job);
        m.next_job
    });
    crate::core::intersection_observer::rendering::queue_task_with_id(scope, cb_job, seq, 0.0);
}

fn cb_job(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let seq = crate::binding::protos::body_data(scope, &args).number_value(scope).unwrap_or(0.0) as u64;
    let Some(job) = with_mse(scope, |m| m.jobs.remove(&seq)) else { return };
    match job {
        Job::Fire(target, ty) => fire(scope, target, ty),
        Job::Append { buffer, op, bytes } => run_append(scope, buffer, op, bytes),
        Job::Remove { buffer, op, start, end } => run_remove(scope, buffer, op, start, end),
    }
}

fn target_object<'s>(scope: &mut v8::PinScope<'s, '_>, target: &Target) -> Option<v8::Local<'s, v8::Object>> {
    let g = with_mse(scope, |m| match target {
        Target::Source(id) => m.sources.get(id).map(|s| s.obj.clone()),
        Target::Buffer(id) => m.buffers.get(id).map(|b| b.obj.clone()),
        Target::List(id, active) => m.sources.get(id).map(|s| if *active { s.active_list.clone() } else { s.list.clone() }),
    })?;
    Some(v8::Local::new(scope, &g))
}

fn fire(scope: &mut v8::PinScope, target: Target, ty: &str) {
    if let Some(o) = target_object(scope, &target) {
        crate::core::html::browsing_context::fire_at(scope, o.into(), ty);
    }
}

fn throw_dom(scope: &mut v8::PinScope, msg: &str, name: &str) {
    if let Some(e) = make_dom_exc(scope, msg, name) {
        scope.throw_exception(e);
    }
}

// ---- the source -----------------------------------------------------------------------------------

/// Register a new `MediaSource`; its id goes in its state slot.
pub(crate) fn new_source(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, list: v8::Local<v8::Object>, active_list: v8::Local<v8::Object>) -> u32 {
    let obj = v8::Global::new(scope, obj);
    let list = v8::Global::new(scope, list);
    let active_list = v8::Global::new(scope, active_list);
    with_mse(scope, |m| {
        m.next += 1;
        let id = m.next;
        m.sources.insert(id, Source { obj, state: ReadyState::Closed, duration: f64::NAN, element: None, buffers: Vec::new(), active: Vec::new(), list, active_list });
        id
    })
}

pub(crate) fn source_state(scope: &mut v8::PinScope, id: u32) -> ReadyState {
    with_mse(scope, |m| m.sources.get(&id).map_or(ReadyState::Closed, |s| s.state))
}

pub(crate) fn source_duration(scope: &mut v8::PinScope, id: u32) -> f64 {
    with_mse(scope, |m| m.sources.get(&id).map_or(f64::NAN, |s| s.duration))
}

/// The source a `MediaSource` object is, if it is one.
pub(crate) fn source_of(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<u32> {
    let s = st::state(scope, v, crate::core::media::media_source::MSRC)?;
    let n = st::get_num(scope, s, "id");
    (n > 0.0).then_some(n as u32)
}

/// A media element attaches the source its object URL names: the source opens (`sourceopen`). `false` when it
/// cannot be attached -- not closed (attached elsewhere already) -- and the element fails as it would on a bad URL.
pub(crate) fn attach(scope: &mut v8::PinScope, source: u32, element: u32) -> bool {
    let ok = with_mse(scope, |m| match m.sources.get_mut(&source) {
        Some(s) if s.state == ReadyState::Closed => {
            s.state = ReadyState::Open;
            s.element = Some(element);
            true
        }
        _ => false,
    });
    if ok {
        queue(scope, Job::Fire(Target::Source(source), "sourceopen"));
    }
    ok
}

/// The element loads something else: the source closes, its buffers go (`removesourcebuffer` for each, from the active
/// list and the list), and `sourceclose`.
pub(crate) fn detach(scope: &mut v8::PinScope, source: u32) {
    let buffers = with_mse(scope, |m| {
        let Some(s) = m.sources.get_mut(&source) else { return None };
        s.state = ReadyState::Closed;
        s.duration = f64::NAN;
        s.element = None;
        s.active.clear();
        let buffers = std::mem::take(&mut s.buffers);
        for b in &buffers {
            if let Some(b) = m.buffers.get_mut(b) {
                b.source = None;
                b.updating = false;
                b.op += 1;
            }
        }
        Some(buffers)
    });
    let Some(buffers) = buffers else { return };
    sync_lists(scope, source);
    for _ in &buffers {
        queue(scope, Job::Fire(Target::List(source, true), "removesourcebuffer"));
        queue(scope, Job::Fire(Target::List(source, false), "removesourcebuffer"));
    }
    queue(scope, Job::Fire(Target::Source(source), "sourceclose"));
}

/// Write a source's buffer lists into its two `SourceBufferList` objects: their `length`, and an own index property per
/// buffer (the lists' indexed getter).
fn sync_lists(scope: &mut v8::PinScope, source: u32) {
    let Some((list, active, all, act)) = with_mse(scope, |m| {
        let s = m.sources.get(&source)?;
        let objs = |ids: &[u32]| ids.iter().filter_map(|b| m.buffers.get(b).map(|b| b.obj.clone())).collect::<Vec<_>>();
        Some((s.list.clone(), s.active_list.clone(), objs(&s.buffers), objs(&s.active)))
    }) else {
        return;
    };
    for (list, items) in [(list, all), (active, act)] {
        let list = v8::Local::new(scope, &list);
        let items: Vec<v8::Local<v8::Value>> = items.iter().map(|g| v8::Local::new(scope, g).into()).collect();
        let Some(s) = st::state(scope, list.into(), crate::core::media::media_source::SBL) else { continue };
        let old = st::get_array(scope, s, "buffers").map_or(0, |a| a.length());
        let arr = st::array(scope, &items);
        st::set(scope, s, "buffers", arr.into());
        for i in 0..old.max(items.len() as u32) {
            let k = v8::String::new(scope, &i.to_string()).unwrap();
            let _ = list.delete(scope, k.into());
            if let Some(v) = items.get(i as usize) {
                list.define_own_property(scope, k.into(), *v, v8::PropertyAttribute::READ_ONLY);
            }
        }
    }
}

/// `addSourceBuffer(type)` past its checks: a new buffer for `mime`, in the source's list (`addsourcebuffer`).
pub(crate) fn add_source_buffer<'s>(scope: &mut v8::PinScope<'s, '_>, source: u32, mime: &str) -> Option<v8::Local<'s, v8::Object>> {
    let proto = crate::binding::protos::shell_proto(scope, SB)?;
    let obj = crate::binding::protos::mint_on(scope, proto.into());
    let parser = demux::Parser::for_mime(mime).unwrap_or_default();
    let g = v8::Global::new(scope, obj);
    let id = with_mse(scope, |m| {
        m.next += 1;
        let id = m.next;
        m.buffers.insert(
            id,
            Buffer {
                obj: g,
                source: Some(source),
                parser,
                updating: false,
                op: 0,
                sequence: false,
                offset: 0.0,
                window: (0.0, f64::INFINITY),
                tracks: HashMap::new(),
                first_init: false,
                group_end: None,
            },
        );
        if let Some(s) = m.sources.get_mut(&source) {
            s.buffers.push(id);
        }
        id
    });
    let s = st::attach(scope, obj, SB);
    st::set_num(scope, s, "id", id as f64);
    sync_lists(scope, source);
    queue(scope, Job::Fire(Target::List(source, false), "addsourcebuffer"));
    Some(obj)
}

fn buffer_of(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<u32> {
    let s = st::state(scope, v, SB)?;
    Some(st::get_num(scope, s, "id") as u32)
}

/// `removeSourceBuffer(buffer)`: `false` when the buffer is not this source's.
pub(crate) fn remove_source_buffer(scope: &mut v8::PinScope, source: u32, buffer: v8::Local<v8::Value>) -> bool {
    let Some(b) = buffer_of(scope, buffer) else { return false };
    let found = with_mse(scope, |m| m.sources.get(&source).is_some_and(|s| s.buffers.contains(&b)));
    if !found {
        return false;
    }
    let (was_updating, active) = with_mse(scope, |m| {
        let buf = m.buffers.get_mut(&b);
        let was = buf.map(|buf| {
            buf.source = None;
            buf.op += 1;
            std::mem::replace(&mut buf.updating, false)
        });
        let s = m.sources.get_mut(&source);
        let active = s.map(|s| {
            s.buffers.retain(|x| *x != b);
            let was_active = s.active.contains(&b);
            s.active.retain(|x| *x != b);
            was_active
        });
        (was.unwrap_or(false), active.unwrap_or(false))
    });
    if was_updating {
        queue(scope, Job::Fire(Target::Buffer(b), "abort"));
        queue(scope, Job::Fire(Target::Buffer(b), "updateend"));
    }
    sync_lists(scope, source);
    if active {
        queue(scope, Job::Fire(Target::List(source, true), "removesourcebuffer"));
    }
    queue(scope, Job::Fire(Target::List(source, false), "removesourcebuffer"));
    true
}

fn any_updating(m: &Mse, source: u32) -> bool {
    m.sources.get(&source).is_some_and(|s| s.buffers.iter().any(|b| m.buffers.get(b).is_some_and(|b| b.updating)))
}

/// `duration = d` past the type check: Chrome's refusals, then the duration change.
pub(crate) fn set_duration(scope: &mut v8::PinScope, source: u32, d: f64) {
    let prefix = "Failed to set the 'duration' property on 'MediaSource': ";
    let (state, updating, highest) = with_mse(scope, |m| {
        let s = m.sources.get(&source);
        let highest = s.map_or(0.0, |s| s.buffers.iter().filter_map(|b| m.buffers.get(b)).map(highest_pts).fold(0.0, f64::max));
        (s.map_or(ReadyState::Closed, |s| s.state), any_updating(m, source), highest)
    });
    if state != ReadyState::Open {
        throw_dom(scope, &format!("{prefix}{NOT_OPEN}"), "InvalidStateError");
        return;
    }
    if updating {
        throw_dom(scope, &format!("{prefix}The 'updating' attribute is true on one or more of this MediaSource's SourceBuffers."), "InvalidStateError");
        return;
    }
    if d < highest {
        throw_dom(
            scope,
            &format!("{prefix}Setting duration below highest presentation timestamp of any buffered coded frames is disallowed. Instead, first do asynchronous remove(newDuration, oldDuration) on all sourceBuffers, where newDuration < oldDuration."),
            "InvalidStateError",
        );
        return;
    }
    duration_change(scope, source, d);
}

/// The duration change algorithm: the source's duration, and the element's (`durationchange`).
fn duration_change(scope: &mut v8::PinScope, source: u32, d: f64) {
    let element = with_mse(scope, |m| {
        let s = m.sources.get_mut(&source)?;
        if s.duration == d || (s.duration.is_nan() && d.is_nan()) {
            return None;
        }
        s.duration = d;
        s.element
    });
    if let Some(e) = element {
        crate::core::html::media_load::mse_duration(scope, e, d);
    }
}

fn highest_pts(b: &Buffer) -> f64 {
    b.tracks.values().flat_map(|t| t.frames.iter().map(|f| f.1)).fold(0.0, f64::max)
}

impl TrackBuf {
    /// The buffered ranges: the frames' intervals, joined across a gap shorter than twice the longest frame.
    fn ranges(&self) -> Vec<(f64, f64)> {
        let fudge = (self.max_frame * 2.0).max(1e-3);
        let mut out: Vec<(f64, f64)> = Vec::new();
        for &(s, e, _) in &self.frames {
            match out.last_mut() {
                Some(last) if s <= last.1 + fudge => last.1 = last.1.max(e),
                _ => out.push((s, e)),
            }
        }
        out
    }

    /// Remove the frames presented in `[start, end)` and, of a video track, the frames after them that depend on them
    /// -- up to the next keyframe (MSE's coded frame removal; Chromium's `SourceBufferStream`).
    fn remove(&mut self, start: f64, end: f64) {
        let video = self.kind == demux::Kind::Video;
        let mut dropping = false;
        self.frames.retain(|&(s, _, key)| {
            if s >= start && s < end {
                dropping = video;
                return false;
            }
            if key || s < start {
                dropping = false;
            }
            !dropping
        });
    }

    fn insert(&mut self, frame: (f64, f64, bool)) {
        let at = self.frames.partition_point(|f| f.0 < frame.0);
        self.frames.insert(at, frame);
    }
}

/// `endOfStream(error)` past its checks.
pub(crate) fn end_of_stream(scope: &mut v8::PinScope, source: u32, error: Option<&str>) -> bool {
    let prefix = "Failed to execute 'endOfStream' on 'MediaSource': ";
    let (state, updating) = with_mse(scope, |m| (m.sources.get(&source).map_or(ReadyState::Closed, |s| s.state), any_updating(m, source)));
    if state != ReadyState::Open {
        throw_dom(scope, &format!("{prefix}{NOT_OPEN}"), "InvalidStateError");
        return false;
    }
    if updating {
        throw_dom(scope, &format!("{prefix}The 'updating' attribute is true on one or more of this MediaSource's SourceBuffers."), "InvalidStateError");
        return false;
    }
    end_of_stream_algorithm(scope, source, error);
    true
}

fn end_of_stream_algorithm(scope: &mut v8::PinScope, source: u32, error: Option<&str>) {
    let (element, highest) = with_mse(scope, |m| {
        let highest = m.sources.get(&source).map_or(0.0, |s| s.buffers.iter().filter_map(|b| m.buffers.get(b)).map(highest_pts).fold(0.0, f64::max));
        let s = m.sources.get_mut(&source);
        let e = s.and_then(|s| {
            s.state = ReadyState::Ended;
            s.element
        });
        (e, highest)
    });
    // every frame is here: the duration is where the buffered frames end (its `durationchange` goes before
    // `sourceended`, as Chrome 150 fires them)
    if error.is_none() && highest > 0.0 {
        duration_change(scope, source, (highest * 1e6).round() / 1e6);
    }
    queue(scope, Job::Fire(Target::Source(source), "sourceended"));
    if let Some(e) = element {
        match error {
            None => crate::core::html::media_load::mse_update(scope, e),
            Some(kind) => crate::core::html::media_load::mse_error(scope, e, kind == "network"),
        }
    }
}

/// An `ended` source that is appended to (or whose buffer is changed) opens again (`sourceopen`).
fn reopen(scope: &mut v8::PinScope, source: u32) {
    let reopened = with_mse(scope, |m| match m.sources.get_mut(&source) {
        Some(s) if s.state == ReadyState::Ended => {
            s.state = ReadyState::Open;
            true
        }
        _ => false,
    });
    if reopened {
        queue(scope, Job::Fire(Target::Source(source), "sourceopen"));
    }
}

// ---- buffered ranges ------------------------------------------------------------------------------

fn intersect(a: &[(f64, f64)], b: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        let s = a[i].0.max(b[j].0);
        let e = a[i].1.min(b[j].1);
        if e > s {
            out.push((s, e));
        }
        if a[i].1 < b[j].1 {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// A buffer's buffered ranges: its tracks' intersected; once ended, each track's last range reaches the highest end.
fn buffer_ranges(b: &Buffer, ended: bool, highest: f64) -> Vec<(f64, f64)> {
    let mut lists = b.tracks.values().filter(|t| t.kind != demux::Kind::Other).map(|t| {
        let mut r = t.ranges();
        if ended {
            if let Some(last) = r.last_mut() {
                last.1 = last.1.max(highest);
            }
        }
        r
    });
    let Some(first) = lists.next() else { return Vec::new() };
    lists.fold(first, |acc, r| intersect(&acc, &r))
}

/// Rounded to the microsecond, as Chrome's `base::TimeDelta` holds every time.
fn micro(ranges: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    ranges.into_iter().map(|(s, e)| ((s * 1e6).round() / 1e6, (e * 1e6).round() / 1e6)).collect()
}

fn ranges_for_buffer(m: &Mse, id: u32) -> Vec<(f64, f64)> {
    let Some(b) = m.buffers.get(&id) else { return Vec::new() };
    let source = b.source.and_then(|s| m.sources.get(&s));
    let ended = source.is_some_and(|s| s.state == ReadyState::Ended);
    let highest = source.map_or(0.0, |s| s.buffers.iter().filter_map(|b| m.buffers.get(b)).map(highest_pts).fold(0.0, f64::max));
    micro(buffer_ranges(b, ended, highest))
}

/// The element's `buffered`: the intersection of the source's active buffers'.
pub(crate) fn element_buffered(scope: &mut v8::PinScope, source: u32) -> Vec<(f64, f64)> {
    with_mse(scope, |m| {
        let Some(s) = m.sources.get(&source) else { return Vec::new() };
        let mut lists = s.active.iter().map(|b| ranges_for_buffer(m, *b));
        let Some(first) = lists.next() else { return Vec::new() };
        lists.fold(first, |acc, r| intersect(&acc, &r))
    })
}

// ---- appending ------------------------------------------------------------------------------------

/// The segment parser loop and the coded frame processing over one append's bytes.
fn run_append(scope: &mut v8::PinScope, buffer: u32, op: u64, bytes: Vec<u8>) {
    let parsed = with_mse(scope, |m| {
        let b = m.buffers.get_mut(&buffer)?;
        if b.op != op || !b.updating {
            return None;
        }
        Some((b.parser.push(&bytes), b.source))
    });
    let Some((parsed, source)) = parsed else { return };
    let events = match parsed {
        Ok(e) => e,
        Err(_) => {
            append_error(scope, buffer, source);
            return;
        }
    };
    let element = source.and_then(|s| with_mse(scope, |m| m.sources.get(&s).and_then(|s| s.element)));
    let mut video = Vec::new();
    let mut ranges_changed = false;
    for e in events {
        match e {
            demux::Event::Init(init) => init_segment(scope, buffer, source, element, init),
            demux::Event::Samples(samples) => {
                ranges_changed = true;
                video.extend(coded_frames(scope, buffer, samples));
            }
        }
    }
    with_mse(scope, |m| {
        if let Some(b) = m.buffers.get_mut(&buffer) {
            b.updating = false;
        }
    });
    // frames past the duration lengthen it to where they end (the coded frame processing's last step)
    if let (Some(source), true) = (source, ranges_changed) {
        let (duration, highest) = with_mse(scope, |m| {
            let s = m.sources.get(&source);
            let highest = s.map_or(0.0, |s| s.buffers.iter().filter_map(|b| m.buffers.get(b)).map(highest_pts).fold(0.0, f64::max));
            (s.map_or(f64::NAN, |s| s.duration), highest)
        });
        let highest = (highest * 1e6).round() / 1e6;
        if duration.is_finite() && highest > duration {
            duration_change(scope, source, highest);
        }
    }
    if let Some(e) = element {
        if !video.is_empty() {
            crate::core::html::media_load::mse_frames(scope, e, video);
        }
        if ranges_changed {
            crate::core::html::media_load::mse_update(scope, e);
        }
    }
    fire(scope, Target::Buffer(buffer), "update");
    fire(scope, Target::Buffer(buffer), "updateend");
}

/// The append error algorithm: the parser starts over, `error` and `updateend`, then the end of stream with a decode
/// error.
fn append_error(scope: &mut v8::PinScope, buffer: u32, source: Option<u32>) {
    with_mse(scope, |m| {
        if let Some(b) = m.buffers.get_mut(&buffer) {
            b.parser.reset();
            b.updating = false;
        }
    });
    fire(scope, Target::Buffer(buffer), "error");
    fire(scope, Target::Buffer(buffer), "updateend");
    if let Some(s) = source {
        if source_state(scope, s) == ReadyState::Open {
            end_of_stream_algorithm(scope, s, Some("decode"));
        }
    }
}

/// An initialization segment: the buffer's tracks, the source's duration if it had none, the buffer made active, and
/// -- once every buffer has had its first -- the element's metadata.
fn init_segment(scope: &mut v8::PinScope, buffer: u32, source: Option<u32>, element: Option<u32>, init: demux::Init) {
    let first = with_mse(scope, |m| {
        let b = m.buffers.get_mut(&buffer)?;
        for t in &init.tracks {
            let tb = b.tracks.entry(t.id).or_insert(TrackBuf { kind: t.kind, frames: Vec::new(), max_frame: 0.0, codec: None });
            if t.kind == demux::Kind::Video {
                tb.codec = Some(Arc::new(t.codec.clone()));
            }
        }
        Some(!std::mem::replace(&mut b.first_init, true))
    });
    if first != Some(true) {
        return;
    }
    let Some(source) = source else { return };
    if source_duration(scope, source).is_nan() {
        duration_change(scope, source, init.duration.unwrap_or(f64::INFINITY));
    }
    let activated = with_mse(scope, |m| {
        let s = m.sources.get_mut(&source)?;
        (!s.active.contains(&buffer)).then(|| s.active.push(buffer))
    });
    if activated.is_some() {
        sync_lists(scope, source);
        queue(scope, Job::Fire(Target::List(source, true), "addsourcebuffer"));
    }
    let all_init = with_mse(scope, |m| m.sources.get(&source).is_some_and(|s| s.buffers.iter().all(|b| m.buffers.get(b).is_some_and(|b| b.first_init))));
    if let (true, Some(e)) = (all_init, element) {
        let video = with_mse(scope, |m| {
            m.sources.get(&source).and_then(|s| {
                s.buffers.iter().filter_map(|b| m.buffers.get(b)).flat_map(|b| b.tracks.values()).find_map(|t| t.codec.clone())
            })
        });
        let size = init.tracks.iter().find(|t| t.kind == demux::Kind::Video).map(|t| (t.width, t.height));
        let duration = source_duration(scope, source);
        crate::core::html::media_load::mse_metadata(scope, e, duration, size.unwrap_or((0, 0)), video);
    }
}

/// The coded frame processing: each frame moved by `timestampOffset` (and, in `sequence` mode, laid after the last
/// group), dropped outside the append window, added to its track's ranges. The video frames, for the decoder.
fn coded_frames(scope: &mut v8::PinScope, buffer: u32, samples: Vec<demux::Sample>) -> Vec<crate::core::media::video_pipe::Coded> {
    let offset_now = with_mse(scope, |m| m.buffers.get(&buffer).map(|b| (b.sequence, b.group_end, b.offset)));
    let Some((sequence, group_end, mut offset)) = offset_now else { return Vec::new() };
    if sequence {
        // the group starts where the last ended (at 0 for the first)
        let first = samples.iter().map(|s| s.pts).fold(f64::INFINITY, f64::min);
        if first.is_finite() {
            offset = group_end.unwrap_or(0.0) - first;
        }
    }
    with_mse(scope, |m| {
        let Some(b) = m.buffers.get_mut(&buffer) else { return Vec::new() };
        b.offset = offset;
        let mut video = Vec::new();
        let mut end_max = b.group_end.unwrap_or(f64::NEG_INFINITY);
        let samples: Vec<demux::Sample> = samples
            .into_iter()
            .filter(|s| s.pts + offset >= b.window.0 && s.pts + offset + s.duration <= b.window.1 && b.tracks.contains_key(&s.track))
            .collect();
        // the frames these overlap go, with what depends on them
        let mut spans: HashMap<u64, (f64, f64)> = HashMap::new();
        for s in &samples {
            let e = spans.entry(s.track).or_insert((f64::INFINITY, f64::NEG_INFINITY));
            e.0 = e.0.min(s.pts + offset);
            e.1 = e.1.max(s.pts + offset + s.duration);
        }
        for (track, (lo, hi)) in spans {
            if let Some(t) = b.tracks.get_mut(&track) {
                t.remove(lo, hi);
            }
        }
        for s in samples {
            let pts = s.pts + offset;
            let dts = s.dts + offset;
            let end = pts + s.duration;
            let Some(t) = b.tracks.get_mut(&s.track) else { continue };
            t.max_frame = t.max_frame.max(s.duration);
            t.insert((pts, end.max(pts + 1e-6), s.key));
            end_max = end_max.max(end);
            if let (Some(data), Some(codec)) = (s.data, t.codec.clone()) {
                video.push(crate::core::media::video_pipe::Coded { dts, pts, duration: s.duration, key: s.key, data, codec });
            }
        }
        if end_max.is_finite() {
            b.group_end = Some(end_max);
        }
        video
    })
}

fn run_remove(scope: &mut v8::PinScope, buffer: u32, op: u64, start: f64, end: f64) {
    let element = with_mse(scope, |m| {
        let b = m.buffers.get_mut(&buffer)?;
        if b.op != op || !b.updating {
            return None;
        }
        for t in b.tracks.values_mut() {
            t.remove(start, end);
        }
        b.updating = false;
        Some(b.source.and_then(|s| m.sources.get(&s)).and_then(|s| s.element))
    });
    let Some(element) = element else { return };
    if let Some(e) = element {
        crate::core::html::media_load::mse_removed(scope, e, start, end);
        crate::core::html::media_load::mse_update(scope, e);
    }
    fire(scope, Target::Buffer(buffer), "update");
    fire(scope, Target::Buffer(buffer), "updateend");
}

// ---- SourceBuffer ---------------------------------------------------------------------------------

/// The receiver's buffer, or `Illegal invocation` thrown.
fn this_buffer(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> Option<u32> {
    let s = st::state_or_throw(scope, args.this().into(), SB)?;
    Some(st::get_num(scope, s, "id") as u32)
}

/// The checks every mutating member makes first: removed, then updating. `what` is Blink's phrase for the member.
fn usable(scope: &mut v8::PinScope, b: u32, what: &str, check_updating: bool) -> Option<Option<u32>> {
    let (source, updating) = with_mse(scope, |m| m.buffers.get(&b).map_or((None, false), |b| (b.source, b.updating)));
    let Some(source) = source else {
        throw_dom(scope, &format!("Failed to {what} 'SourceBuffer': {REMOVED}"), "InvalidStateError");
        return None;
    };
    if check_updating && updating {
        throw_dom(scope, &format!("Failed to {what} 'SourceBuffer': {STILL_UPDATING}"), "InvalidStateError");
        return None;
    }
    Some(Some(source))
}

fn cb_sb_append(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(b) = this_buffer(scope, &args) else { return };
    let Some(bytes) = crate::core::streams::text_decoder_stream::buffer_source_bytes(args.get(0)) else {
        throw_type_error(scope, "Failed to execute 'appendBuffer' on 'SourceBuffer': The provided value is not of type '(ArrayBuffer or ArrayBufferView)'.");
        return;
    };
    let Some(Some(source)) = usable(scope, b, "execute 'appendBuffer' on", true) else { return };
    reopen(scope, source);
    let op = with_mse(scope, |m| {
        let buf = m.buffers.get_mut(&b)?;
        buf.updating = true;
        Some(buf.op)
    });
    let Some(op) = op else { return };
    queue(scope, Job::Fire(Target::Buffer(b), "updatestart"));
    queue(scope, Job::Append { buffer: b, op, bytes });
}

fn cb_sb_remove(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(b) = this_buffer(scope, &args) else { return };
    let start = args.get(0).number_value(scope).unwrap_or(f64::NAN);
    let end = args.get(1).number_value(scope).unwrap_or(f64::NAN);
    if !start.is_finite() {
        throw_type_error(scope, "Failed to execute 'remove' on 'SourceBuffer': The provided double value is non-finite.");
        return;
    }
    let Some(Some(source)) = usable(scope, b, "execute 'remove' on", true) else { return };
    let duration = source_duration(scope, source);
    if duration.is_nan() || start < 0.0 || start > duration {
        throw_type_error(scope, &format!("Failed to execute 'remove' on 'SourceBuffer': The start provided ({}) is outside the range (0, {}).", num(start), num(duration)));
        return;
    }
    if end.is_nan() || end <= start {
        throw_type_error(scope, &format!("Failed to execute 'remove' on 'SourceBuffer': The end value provided ({}) must be greater than the start value provided ({}).", num(end), num(start)));
        return;
    }
    if source_state(scope, source) == ReadyState::Ended {
        reopen(scope, source);
    }
    let op = with_mse(scope, |m| {
        let buf = m.buffers.get_mut(&b)?;
        buf.updating = true;
        Some(buf.op)
    });
    let Some(op) = op else { return };
    queue(scope, Job::Fire(Target::Buffer(b), "updatestart"));
    queue(scope, Job::Remove { buffer: b, op, start, end });
}

/// A number as JavaScript prints it.
pub(crate) fn num(v: f64) -> String {
    if v.is_nan() {
        "NaN".into()
    } else if v.is_infinite() {
        if v > 0.0 { "Infinity".into() } else { "-Infinity".into() }
    } else if v == v.trunc() && v.abs() < 1e21 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

fn cb_sb_abort(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(b) = this_buffer(scope, &args) else { return };
    let Some(Some(source)) = usable(scope, b, "execute 'abort' on", false) else { return };
    if source_state(scope, source) != ReadyState::Open {
        throw_dom(scope, &format!("Failed to execute 'abort' on 'SourceBuffer': {NOT_OPEN}"), "InvalidStateError");
        return;
    }
    let was = with_mse(scope, |m| {
        let buf = m.buffers.get_mut(&b)?;
        buf.op += 1;
        buf.parser.reset();
        buf.window = (0.0, f64::INFINITY);
        Some(std::mem::replace(&mut buf.updating, false))
    });
    if was == Some(true) {
        fire(scope, Target::Buffer(b), "abort");
        fire(scope, Target::Buffer(b), "updateend");
    }
}

fn cb_sb_change_type(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(b) = this_buffer(scope, &args) else { return };
    let Some(t) = st::to_str(scope, args.get(0)) else { return };
    if t.is_empty() {
        throw_type_error(scope, "Failed to execute 'changeType' on 'SourceBuffer': The type provided is empty");
        return;
    }
    let Some(Some(source)) = usable(scope, b, "execute 'changeType' on", true) else { return };
    if !crate::core::dom::media_codecs::media_source_supported(scope, &t) {
        throw_dom(scope, &format!("Failed to execute 'changeType' on 'SourceBuffer': Changing to the type provided ('{t}') is not supported."), "NotSupportedError");
        return;
    }
    reopen(scope, source);
    with_mse(scope, |m| {
        if let Some(buf) = m.buffers.get_mut(&b) {
            buf.parser.change_type(&t);
        }
    });
}

fn cb_sb_updating(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(b) = this_buffer(scope, &args) else { return };
    let u = with_mse(scope, |m| m.buffers.get(&b).is_some_and(|b| b.updating));
    rv.set_bool(u);
}

fn cb_sb_buffered(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(b) = this_buffer(scope, &args) else { return };
    let removed = with_mse(scope, |m| m.buffers.get(&b).is_none_or(|b| b.source.is_none()));
    if removed {
        throw_dom(scope, &format!("Failed to read the 'buffered' property from 'SourceBuffer': {REMOVED}"), "InvalidStateError");
        return;
    }
    let ranges = with_mse(scope, |m| ranges_for_buffer(m, b));
    if let Some(o) = crate::core::html::media_objects::mint_time_ranges_with(scope, &ranges) {
        rv.set(o.into());
    }
}

fn cb_sb_mode_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(b) = this_buffer(scope, &args) else { return };
    let seq = with_mse(scope, |m| m.buffers.get(&b).is_some_and(|b| b.sequence));
    let v = str_val(scope, if seq { "sequence" } else { "segments" });
    rv.set(v);
}

fn cb_sb_mode_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(b) = this_buffer(scope, &args) else { return };
    let Some(v) = st::to_str(scope, args.get(0)) else { return };
    // an attribute's enumeration: any other string is ignored
    let sequence = match v.as_str() {
        "segments" => false,
        "sequence" => true,
        _ => return,
    };
    let Some(Some(source)) = usable(scope, b, "set the 'mode' property on", true) else { return };
    reopen(scope, source);
    with_mse(scope, |m| {
        if let Some(buf) = m.buffers.get_mut(&b) {
            buf.sequence = sequence;
            if sequence {
                buf.group_end = None;
            }
        }
    });
}

fn cb_sb_offset_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(b) = this_buffer(scope, &args) else { return };
    let o = with_mse(scope, |m| m.buffers.get(&b).map_or(0.0, |b| b.offset));
    rv.set(v8::Number::new(scope, o).into());
}

fn cb_sb_offset_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(b) = this_buffer(scope, &args) else { return };
    let v = args.get(0).number_value(scope).unwrap_or(f64::NAN);
    if !v.is_finite() {
        throw_type_error(scope, "Failed to set the 'timestampOffset' property on 'SourceBuffer': The provided double value is non-finite.");
        return;
    }
    let Some(Some(source)) = usable(scope, b, "set the 'timestampOffset' property on", true) else { return };
    reopen(scope, source);
    with_mse(scope, |m| {
        if let Some(buf) = m.buffers.get_mut(&b) {
            buf.offset = v;
            if buf.sequence {
                buf.group_end = Some(v);
            }
        }
    });
}

fn cb_sb_window_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(b) = this_buffer(scope, &args) else { return };
    let end = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope) == "end";
    let w = with_mse(scope, |m| m.buffers.get(&b).map_or((0.0, f64::INFINITY), |b| b.window));
    rv.set(v8::Number::new(scope, if end { w.1 } else { w.0 }).into());
}

fn cb_sb_window_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(b) = this_buffer(scope, &args) else { return };
    let end = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope) == "end";
    let v = args.get(0).number_value(scope).unwrap_or(f64::NAN);
    let name = if end { "appendWindowEnd" } else { "appendWindowStart" };
    if !end && !v.is_finite() {
        throw_type_error(scope, &format!("Failed to set the '{name}' property on 'SourceBuffer': The provided double value is non-finite."));
        return;
    }
    let Some(Some(_)) = usable(scope, b, &format!("set the '{name}' property on"), true) else { return };
    let w = with_mse(scope, |m| m.buffers.get(&b).map_or((0.0, f64::INFINITY), |b| b.window));
    if end {
        if v.is_nan() {
            throw_type_error(scope, &format!("Failed to set the '{name}' property on 'SourceBuffer': The value provided is NaN."));
            return;
        }
        if v <= w.0 {
            throw_type_error(scope, &format!("Failed to set the '{name}' property on 'SourceBuffer': The value provided ({}) is less than or equal to the minimum value ({}).", num(v), num(w.0)));
            return;
        }
    } else if v < 0.0 || v >= w.1 {
        throw_type_error(scope, &format!("Failed to set the '{name}' property on 'SourceBuffer': The value provided ({}) is outside the range [0, {}).", num(v), num(w.1)));
        return;
    }
    with_mse(scope, |m| {
        if let Some(buf) = m.buffers.get_mut(&b) {
            if end {
                buf.window.1 = v;
            } else {
                buf.window.0 = v;
            }
        }
    });
}

fn getter(scope: &mut v8::PinScope, target: v8::Local<v8::Object>, member: &'static MemberDef, get: impl v8::MapFnTo<v8::FunctionCallback>) {
    let g = build_fn(scope, get, &format!("get {}", member.name), 0);
    define_accessor(scope, target, member, g, None);
}

fn getter_setter(
    scope: &mut v8::PinScope,
    target: v8::Local<v8::Object>,
    member: &'static MemberDef,
    get: impl v8::MapFnTo<v8::FunctionCallback>,
    set: impl v8::MapFnTo<v8::FunctionCallback>,
    data: &str,
) {
    let d = str_val(scope, data);
    let g = build_fn_data(scope, get, &format!("get {}", member.name), 0, d);
    let s = build_fn_data(scope, set, &format!("set {}", member.name), 1, d);
    define_accessor(scope, target, member, g, Some(s));
}

pub fn install_source_buffer(scope: &mut v8::PinScope, target: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "mode" => getter_setter(scope, target, member, cb_sb_mode_get, cb_sb_mode_set, ""),
        "updating" => getter(scope, target, member, cb_sb_updating),
        "buffered" => getter(scope, target, member, cb_sb_buffered),
        "timestampOffset" => getter_setter(scope, target, member, cb_sb_offset_get, cb_sb_offset_set, ""),
        "appendWindowStart" => getter_setter(scope, target, member, cb_sb_window_get, cb_sb_window_set, "start"),
        "appendWindowEnd" => getter_setter(scope, target, member, cb_sb_window_get, cb_sb_window_set, "end"),
        "appendBuffer" => method_row(scope, target, member, cb_sb_append),
        "remove" => method_row(scope, target, member, cb_sb_remove),
        "abort" => method_row(scope, target, member, cb_sb_abort),
        "changeType" => method_row(scope, target, member, cb_sb_change_type),
        other => debug_assert!(false, "install_source_buffer: unexpected member {other}"),
    }
}

webapi::interface! {
    "SourceBuffer" : "EventTarget" [interface, both] {
    ctor    "SourceBuffer" len 0                         = stub,
    attr    "mode" : Object                              = install_source_buffer,
    attr    "updating" : Object readonly                 = install_source_buffer,
    attr    "buffered" : Object readonly                 = install_source_buffer,
    attr    "timestampOffset" : Object                   = install_source_buffer,
    attr    "appendWindowStart" : Object                 = install_source_buffer,
    attr    "appendWindowEnd" : Object                   = install_source_buffer,
    handler "onupdatestart" nullable                     = event_handler,
    handler "onupdate" nullable                          = event_handler,
    handler "onupdateend" nullable                       = event_handler,
    handler "onerror" nullable                           = event_handler,
    handler "onabort" nullable                           = event_handler,
    method  "abort" : Object                             = install_source_buffer,
    method  "appendBuffer" : Object len 1 required 1     = install_source_buffer,
    method  "changeType" : Object len 1 required 1       = install_source_buffer,
    method  "remove" : Object len 2 required 2           = install_source_buffer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(kind: demux::Kind, frames: &[(f64, bool)]) -> TrackBuf {
        TrackBuf { kind, frames: frames.iter().map(|&(s, k)| (s, s + 0.1, k)).collect(), max_frame: 0.1, codec: None }
    }

    #[test]
    fn frames_join_into_ranges_across_small_gaps() {
        let t = track(demux::Kind::Audio, &[(0.0, true), (0.1, true), (0.25, true), (1.0, true)]);
        let r = t.ranges();
        assert_eq!(r.len(), 2);
        assert!((r[0].1 - 0.35).abs() < 1e-9 && r[1] == (1.0, 1.1), "{r:?}");
    }

    #[test]
    fn a_removal_takes_the_video_frames_up_to_the_next_keyframe() {
        let mut t = track(demux::Kind::Video, &[(0.0, true), (0.1, false), (0.2, false), (0.3, true), (0.4, false)]);
        t.remove(0.0, 0.15);
        assert_eq!(t.frames.iter().map(|f| f.0).collect::<Vec<_>>(), vec![0.3, 0.4]);
        let mut a = track(demux::Kind::Audio, &[(0.0, true), (0.1, true), (0.2, true)]);
        a.remove(0.0, 0.15);
        assert_eq!(a.frames.len(), 1);
        assert_eq!(intersect(&[(0.0, 2.0), (3.0, 5.0)], &[(1.0, 4.0)]), vec![(1.0, 2.0), (3.0, 4.0)]);
    }
}
