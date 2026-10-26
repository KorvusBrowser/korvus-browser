//! `core/html/media_load.rs` -- a media element's LOAD: HTML's media element load algorithm, the
//! resource selection algorithm, the fetch, and the readiness and playback states they drive.
//!
//! WHAT WAS HERE BEFORE: a constant-state model. `networkState` read 0, `readyState` 0, `currentSrc`
//! "", no media event ever fired, and `play()` rejected `NotSupportedError` for every element, a
//! playable one included -- so `video.play()` on a muted hero video was an unhandled rejection on
//! svelte.dev, replicate.com, weebly.com and wolfram.com, where Chrome resolves it. A script-set `src`
//! WAS fetched, by the generic element loader, which then fired `load` at the element -- an event
//! Chrome never fires at a media element.
//!
//! MEASURED on Chrome 150 (fixture `M-media-play`, no user activation), each element created by script,
//! set up, appended, then `play()`ed:
//!
//! ```text
//! muted  src=v.mp4    sync: currentSrc "", networkState 3, readyState 0
//!                     play() resolves; after 1.5 s: /v.mp4, 1, 4, paused (the 0.156 s clip ended)
//!                     play,waiting,loadstart,suspend,loadedmetadata,loadeddata,canplay,playing,pause
//! unmuted src=v.mp4   play() rejects NotAllowedError (autoplay policy); the load runs anyway:
//!                     loadstart,suspend,loadedmetadata,loadeddata,canplay; 1, 4, paused
//! no src              play() rejects NotAllowedError -- the policy is asked FIRST; nothing loads
//! muted  src=bad.xyz  play() rejects NotSupportedError; error.code 4, networkState 3, readyState 0
//!                     play,waiting,loadstart,suspend,error,pause
//! ```
//!
//! THE MODEL. Every step below is Blink's `HTMLMediaElement` step of the same name, in its order:
//! `InvokeLoadAlgorithm`, `InvokeResourceSelectionAlgorithm` (the synchronous half: networkState
//! NO_SOURCE), `SelectMediaResource` (a task: loadstart, `currentSrc`), `LoadResource`, and
//! `SetReadyState`'s event table. Media element tasks are real tasks, one per event, and a run of the
//! load algorithm drops every task an earlier run queued.
//!
//! WHAT IS NOT A PIPELINE. No sample is decoded. The whole resource is fetched (the
//! element loader has no range requests), the container header is read by `media_probe.rs`, and a
//! resource whose header reads is HAVE_ENOUGH_DATA at once -- which is what Chrome reports for a file
//! it has fully buffered. Playback is a clock: `currentTime` advances in real time while the element
//! plays, and the end of the resource arrives when the clock reaches the duration (`timeupdate`,
//! `pause`, `ended`). The periodic `timeupdate` a playing element fires every 250 ms is not modelled.
//!
//! THE AUTOPLAY POLICY is Chrome's desktop default ("document user activation required"): a muted
//! VIDEO may play without a gesture, anything else needs the document to have had user activation --
//! and this engine has no user-activation model (`navigator.userActivation.hasBeenActive` is false),
//! so it never has.
//!
//! A MEDIA ELEMENT DOES NOT DELAY THE DOCUMENT'S `load` (`subresource.rs::delays_load`, unchanged): the
//! fetch here downloads the whole body, where Chrome releases the load event at HAVE_CURRENT_DATA.

use std::collections::HashMap;

use deno_core::v8;

use crate::binding::protos::{build_fn_data, get_prop, make_dom_exc, str_val, unwrap_id, wrap_cached};
use crate::binding::HostDom;
use crate::core::html::browsing_context::fire_at;
use crate::core::html::media_probe::{MediaProbe, MediaProbes};

pub(crate) const NETWORK_EMPTY: u8 = 0;
pub(crate) const NETWORK_IDLE: u8 = 1;
pub(crate) const NETWORK_LOADING: u8 = 2;
pub(crate) const NETWORK_NO_SOURCE: u8 = 3;
pub(crate) const HAVE_NOTHING: u8 = 0;
pub(crate) const HAVE_METADATA: u8 = 1;
pub(crate) const HAVE_CURRENT_DATA: u8 = 2;
pub(crate) const HAVE_FUTURE_DATA: u8 = 3;
pub(crate) const HAVE_ENOUGH_DATA: u8 = 4;

/// Chrome's messages, verbatim.
const NOT_ALLOWED: &str = "play() failed because the user didn't interact with the document first. https://goo.gl/xX8pDD";
const NO_SUPPORTED_SOURCE: &str = "Failed to load because no supported source was found.";
const NO_SUPPORTED_SOURCES_SYNC: &str = "The element has no supported sources.";
const INTERRUPTED_BY_LOAD: &str = "The play() request was interrupted by a new load request. https://goo.gl/LdLk22";
const INTERRUPTED_BY_PAUSE: &str = "The play() request was interrupted by a call to pause(). https://goo.gl/LdLk22";
const FORMAT_ERROR: &str = "MEDIA_ELEMENT_ERROR: Format error";
const DEMUXER_ERROR: &str = "DEMUXER_ERROR_COULD_NOT_OPEN: FFmpegDemuxer: open context failed";
const EMPTY_SRC: &str = "MEDIA_ELEMENT_ERROR: Empty src attribute";

/// Where the resource selection algorithm stands.
#[derive(Clone, PartialEq, Default)]
enum Load {
    #[default]
    Idle,
    FromAttribute,
    /// Loading the resource a `<source>` child named.
    FromSource(u32),
    /// `preload=none`: the fetch waits for `play()`.
    Deferred(String),
    WaitingForSource,
}

/// One media element's state.
pub(crate) struct Media {
    pub(crate) network: u8,
    pub(crate) ready: u8,
    pub(crate) paused: bool,
    pub(crate) current_src: String,
    /// `MediaError` code and message.
    pub(crate) error: Option<(u16, String)>,
    /// The `MediaError` object `error` hands out, so `v.error === v.error`.
    error_obj: Option<v8::Global<v8::Object>>,
    pub(crate) duration: f64,
    pub(crate) size: (u32, u32),
    /// Bumped by every run of the load algorithm; a task or a fetch from an older run is dropped.
    generation: u64,
    load: Load,
    /// The `<source>` child the next candidate search starts from.
    next_source: Option<u32>,
    /// The URL whose response this element is waiting for.
    fetching: Option<String>,
    pending_play: Vec<v8::Global<v8::PromiseResolver>>,
    can_autoplay: bool,
    /// The official playback position; while playing, the position at `since`.
    position: f64,
    since: Option<std::time::Instant>,
    /// Bumped whenever the playback clock stops or restarts, so an armed end-of-resource task knows
    /// whether it is still the one that should fire.
    session: u64,
    /// The furthest position played, for `played`.
    played_to: f64,
    /// HTML's show poster flag: the poster stands for the video until it first plays or seeks.
    show_poster: bool,
    /// The decoding thread of a video whose frames this engine decodes (`video_pipe.rs`).
    pipe: Option<crate::core::media::video_pipe::Pipe>,
    /// The `MediaSource` attached through the resource's object URL (`mse.rs`).
    pub(crate) mse: Option<u32>,
}

impl Default for Media {
    fn default() -> Self {
        Media {
            network: NETWORK_EMPTY,
            ready: HAVE_NOTHING,
            paused: true,
            current_src: String::new(),
            error: None,
            error_obj: None,
            duration: f64::NAN,
            size: (0, 0),
            generation: 0,
            load: Load::Idle,
            next_source: None,
            fetching: None,
            pending_play: Vec::new(),
            can_autoplay: true,
            position: 0.0,
            since: None,
            session: 0,
            played_to: 0.0,
            show_poster: true,
            pipe: None,
            mse: None,
        }
    }
}

/// A media element task: fired from its own task, in the order queued.
enum Task {
    Fire(&'static str),
    /// `error` at a rejected `<source>` candidate.
    FireAtSource(u32),
    /// `SelectMediaResource`: the half of the resource selection algorithm after the stable state.
    Select,
    NextSource,
    /// `playing`, then the play promises taken when it was queued are resolved.
    NotifyPlaying(Vec<v8::Global<v8::PromiseResolver>>),
    Resolve(Vec<v8::Global<v8::PromiseResolver>>),
    Reject(Vec<v8::Global<v8::PromiseResolver>>, &'static str, &'static str),
    /// A failed element that had been told to play stops: `pause` (measured, after `error`).
    PauseAfterError,
    /// The playback clock reached the end of the resource (the session it was armed for).
    ReachEnd(u64),
    /// A playing element's periodic `timeupdate` (Chrome's every 250 ms), for the session it was armed in.
    Tick(u64),
    /// A playing MSE element reached the end of the frames buffered at its position (the session it was armed in).
    Stall(u64),
    /// An MSE element's pipeline has its metadata: HAVE_METADATA, then the readiness its buffered frames give.
    MseMetadata,
}

#[derive(Default)]
struct MediaElements {
    map: HashMap<u32, Media>,
    tasks: HashMap<u64, (u32, u64, Task)>,
    next_task: u64,
}

fn with_all<R>(scope: &mut v8::PinScope, f: impl FnOnce(&mut MediaElements) -> R) -> R {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<MediaElements>().is_none() {
        st.put(MediaElements::default());
    }
    f(st.borrow_mut::<MediaElements>())
}

/// Run `f` on element `id`'s state, creating it on first use.
fn with_media<R>(scope: &mut v8::PinScope, id: u32, f: impl FnOnce(&mut Media) -> R) -> R {
    with_all(scope, |all| f(all.map.entry(id).or_default()))
}

/// Read element `id`'s state without creating it: `None` for an element nothing has loaded.
pub(crate) fn read<R>(scope: &mut v8::PinScope, id: u32, f: impl FnOnce(&Media) -> R) -> Option<R> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<MediaElements>().and_then(|all| all.map.get(&id)).map(f)
}

fn queue(scope: &mut v8::PinScope, id: u32, task: Task) {
    let seq = with_all(scope, |all| {
        let generation = all.map.get(&id).map_or(0, |m| m.generation);
        all.next_task += 1;
        let seq = all.next_task;
        all.tasks.insert(seq, (id, generation, task));
        seq
    });
    crate::core::intersection_observer::rendering::queue_task_with_id(scope, cb_task, seq, 0.0);
}

fn queue_after(scope: &mut v8::PinScope, id: u32, task: Task, ms: f64) {
    let seq = with_all(scope, |all| {
        let generation = all.map.get(&id).map_or(0, |m| m.generation);
        all.next_task += 1;
        let seq = all.next_task;
        all.tasks.insert(seq, (id, generation, task));
        seq
    });
    crate::core::intersection_observer::rendering::queue_task_with_id(scope, cb_task, seq, ms);
}

fn cb_task(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let seq = crate::binding::protos::body_data(scope, &args).number_value(scope).unwrap_or(0.0) as u64;
    let Some((id, generation, task)) = with_all(scope, |all| all.tasks.remove(&seq)) else { return };
    let current = read(scope, id, |m| m.generation).unwrap_or(0);
    if generation == current {
        run(scope, id, task);
        return;
    }
    // A later load dropped this task. One that would have settled play promises settles them anyway:
    // HTML's load algorithm resolves or rejects those "immediately" rather than losing them.
    match task {
        Task::NotifyPlaying(promises) | Task::Resolve(promises) => settle(scope, promises, None),
        Task::Reject(promises, name, msg) => settle(scope, promises, Some((name, msg))),
        _ => {}
    }
}

fn element<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32) -> v8::Local<'s, v8::Object> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    wrap_cached(scope, &op_state, id)
}

fn fire(scope: &mut v8::PinScope, id: u32, ty: &str) {
    let el = element(scope, id);
    fire_at(scope, el.into(), ty);
}

fn run(scope: &mut v8::PinScope, id: u32, task: Task) {
    match task {
        Task::Fire(ty) => fire(scope, id, ty),
        Task::FireAtSource(source) => fire(scope, source, "error"),
        Task::Select => select(scope, id),
        Task::NextSource => load_next_source(scope, id),
        Task::NotifyPlaying(promises) => {
            start_clock(scope, id);
            fire(scope, id, "playing");
            settle(scope, promises, None);
        }
        Task::Resolve(promises) => settle(scope, promises, None),
        Task::Reject(promises, name, msg) => settle(scope, promises, Some((name, msg))),
        Task::PauseAfterError => {
            let was_playing = with_media(scope, id, |m| !std::mem::replace(&mut m.paused, true));
            if was_playing {
                stop_clock(scope, id);
                fire(scope, id, "pause");
            }
        }
        Task::ReachEnd(session) => reach_end(scope, id, session),
        Task::Tick(session) => {
            if read(scope, id, |m| m.session == session && m.since.is_some()).unwrap_or(false) {
                fire(scope, id, "timeupdate");
                queue_after(scope, id, Task::Tick(session), TIMEUPDATE_MS);
            }
        }
        Task::Stall(session) => stall(scope, id, session),
        Task::MseMetadata => {
            if read(scope, id, |m| m.ready).unwrap_or(HAVE_NOTHING) < HAVE_METADATA {
                set_ready(scope, id, HAVE_METADATA);
            }
            mse_update(scope, id);
        }
    }
}

/// How often a playing element fires `timeupdate` (Chrome's `kMaxTimeupdateEventFrequency`).
const TIMEUPDATE_MS: f64 = 250.0;

fn settle(scope: &mut v8::PinScope, promises: Vec<v8::Global<v8::PromiseResolver>>, error: Option<(&str, &str)>) {
    for g in promises {
        let r = v8::Local::new(scope, &g);
        match error {
            None => {
                let u = v8::undefined(scope);
                r.resolve(scope, u.into());
            }
            Some((name, msg)) => {
                if let Some(e) = make_dom_exc(scope, msg, name) {
                    r.reject(scope, e);
                }
            }
        }
    }
}

// ---- reading the element --------------------------------------------------------------------------

fn dom_read<R>(scope: &mut v8::PinScope, f: impl FnOnce(&korvus_dom::Dom) -> R) -> Option<R> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<HostDom>().map(|h| f(&h.0))
}

fn attr(scope: &mut v8::PinScope, id: u32, name: &str) -> Option<String> {
    dom_read(scope, |d| d.get_attribute(id, name).map(str::to_string)).flatten()
}

fn is_video(scope: &mut v8::PinScope, id: u32) -> bool {
    dom_read(scope, |d| d.tag_name(id).is_some_and(|t| t.eq_ignore_ascii_case("video"))).unwrap_or(false)
}

/// Is `id` a `<video>` or an `<audio>`?
pub(crate) fn is_media(dom: &korvus_dom::Dom, id: u32) -> bool {
    dom.tag_name(id).is_some_and(|t| t.eq_ignore_ascii_case("video") || t.eq_ignore_ascii_case("audio"))
}

fn first_source(scope: &mut v8::PinScope, id: u32) -> Option<u32> {
    dom_read(scope, |d| d.child_elements(id).into_iter().find(|&c| d.tag_name(c).is_some_and(|t| t.eq_ignore_ascii_case("source")))).flatten()
}

fn muted(scope: &mut v8::PinScope, id: u32) -> bool {
    let el = element(scope, id);
    crate::core::html::html_media_element::is_muted(scope, el)
}

/// Chrome's desktop autoplay policy with no user activation: only a muted video plays.
fn autoplay_allowed(scope: &mut v8::PinScope, id: u32) -> bool {
    is_video(scope, id) && muted(scope, id)
}

fn has_src_object(scope: &mut v8::PinScope, id: u32) -> bool {
    let el = element(scope, id);
    crate::core::html::html_media_element::src_object(scope, el).is_some()
}

fn resolve_url(scope: &mut v8::PinScope, id: u32, raw: &str) -> Option<String> {
    let base = crate::core::html::subresource::node_base_url(scope, id);
    url::Url::parse(&base).ok()?.join(raw).ok().map(|u| u.to_string())
}

// ---- the algorithms -------------------------------------------------------------------------------

/// A `src` attribute was set or changed on `id`: the media element load algorithm. Removing it
/// starts nothing.
pub(crate) fn src_set(scope: &mut v8::PinScope, id: u32) {
    if attr(scope, id, "src").is_some() {
        load_algorithm(scope, id);
    }
}

/// A media element the PARSER made: its `src` was set at creation, and its `<source>` children were
/// inserted into it -- either one starts the load, exactly as the same markup inserted by script does.
pub(crate) fn parsed(scope: &mut v8::PinScope, id: u32) {
    if read(scope, id, |_| ()).is_some() {
        return;
    }
    if attr(scope, id, "src").is_some() {
        load_algorithm(scope, id);
    } else if first_source(scope, id).is_some() {
        with_media(scope, id, |_| ());
        resource_selection(scope, id);
    }
}

/// A `<source>` was inserted into media element `media` (Blink `SourceWasAdded`).
pub(crate) fn source_inserted(scope: &mut v8::PinScope, media: u32, source: u32) {
    if attr(scope, media, "src").is_some() {
        return;
    }
    let (network, load, next) = with_media(scope, media, |m| (m.network, m.load.clone(), m.next_source));
    if network == NETWORK_EMPTY {
        resource_selection(scope, media);
        with_media(scope, media, |m| m.next_source = Some(source));
        return;
    }
    if next.is_some() || load != Load::WaitingForSource {
        return;
    }
    with_media(scope, media, |m| {
        m.network = NETWORK_LOADING;
        m.next_source = Some(source);
    });
    queue(scope, media, Task::NextSource);
}

/// HTML's media element load algorithm (Blink `InvokeLoadAlgorithm`).
pub(crate) fn load_algorithm(scope: &mut v8::PinScope, id: u32) {
    // Abort the running selection and drop every task it queued.
    let network = with_media(scope, id, |m| {
        m.generation += 1;
        m.load = Load::Idle;
        m.next_source = None;
        m.network
    });
    if network == NETWORK_LOADING || network == NETWORK_IDLE {
        queue(scope, id, Task::Fire("abort"));
    }
    if network != NETWORK_EMPTY {
        queue(scope, id, Task::Fire("emptied"));
        let rejected = with_media(scope, id, |m| {
            m.network = NETWORK_EMPTY;
            m.fetching = None;
            m.ready = HAVE_NOTHING;
            m.duration = f64::NAN;
            m.size = (0, 0);
            m.since = None;
            m.session += 1;
            m.position = 0.0;
            m.played_to = 0.0;
            if m.paused {
                Vec::new()
            } else {
                m.paused = true;
                std::mem::take(&mut m.pending_play)
            }
        });
        settle(scope, rejected, Some(("AbortError", INTERRUPTED_BY_LOAD)));
        queue(scope, id, Task::Fire("timeupdate"));
    }
    let el = element(scope, id);
    crate::core::html::html_media_element::reset_playback_rate(scope, el);
    with_media(scope, id, |m| {
        m.error = None;
        m.error_obj = None;
        m.can_autoplay = true;
        m.show_poster = true;
        m.pipe = None;
    });
    if let Some(source) = with_media(scope, id, |m| m.mse.take()) {
        crate::core::media::mse::detach(scope, source);
    }
    resource_selection(scope, id);
}

/// The resource selection algorithm's synchronous half: NO_SOURCE now, the rest after a stable state.
fn resource_selection(scope: &mut v8::PinScope, id: u32) {
    with_media(scope, id, |m| {
        m.network = NETWORK_NO_SOURCE;
        m.duration = f64::NAN;
        m.load = Load::Idle;
    });
    queue(scope, id, Task::Select);
}

/// `SelectMediaResource`.
fn select(scope: &mut v8::PinScope, id: u32) {
    let object = has_src_object(scope, id);
    let src = attr(scope, id, "src");
    let source = if object || src.is_some() { None } else { first_source(scope, id) };
    if !object && src.is_none() && source.is_none() {
        with_media(scope, id, |m| {
            m.load = Load::WaitingForSource;
            m.network = NETWORK_EMPTY;
        });
        return;
    }
    with_media(scope, id, |m| m.network = NETWORK_LOADING);
    queue(scope, id, Task::Fire("loadstart"));
    if object {
        // A MediaStream or MediaSource object: nothing here can play one, and nothing is fetched.
        return;
    }
    if let Some(raw) = src {
        let raw = raw.trim().to_string();
        if raw.is_empty() {
            with_media(scope, id, |m| m.load = Load::FromAttribute);
            loading_failed(scope, id, EMPTY_SRC);
            return;
        }
        match resolve_url(scope, id, &raw) {
            Some(url) => load_resource(scope, id, url, Load::FromAttribute),
            None => {
                with_media(scope, id, |m| m.load = Load::FromAttribute);
                loading_failed(scope, id, FORMAT_ERROR);
            }
        }
        return;
    }
    with_media(scope, id, |m| m.next_source = source);
    load_next_source(scope, id);
}

/// The next acceptable `<source>` child from the element's pointer, queuing `error` at every
/// candidate passed over (Blink `SelectNextSourceChild`).
fn load_next_source(scope: &mut v8::PinScope, id: u32) {
    let start = with_media(scope, id, |m| m.next_source);
    let children: Vec<u32> = dom_read(scope, |d| d.child_elements(id)).unwrap_or_default();
    let mut looking = start.is_some();
    for child in children {
        if looking && Some(child) != start {
            continue;
        }
        looking = false;
        let is_source = dom_read(scope, |d| d.tag_name(child).is_some_and(|t| t.eq_ignore_ascii_case("source"))).unwrap_or(false);
        if !is_source {
            continue;
        }
        if let Some(url) = acceptable_source(scope, child) {
            let next = dom_read(scope, |d| d.next_sibling(child)).flatten();
            with_media(scope, id, |m| m.next_source = next);
            load_resource(scope, id, url, Load::FromSource(child));
            return;
        }
        queue(scope, id, Task::FireAtSource(child));
    }
    with_media(scope, id, |m| m.next_source = None);
    wait_for_source(scope, id);
}

/// A `<source>`'s URL, when it names one this element would try: a non-empty `src`, a `type` the
/// browser can play (`canPlayType` not ""), and a `media` query that matches.
fn acceptable_source(scope: &mut v8::PinScope, source: u32) -> Option<String> {
    let raw = attr(scope, source, "src")?.trim().to_string();
    if raw.is_empty() {
        return None;
    }
    let url = resolve_url(scope, source, &raw)?;
    if let Some(ty) = attr(scope, source, "type").map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) {
        if crate::core::dom::media_codecs::can_play_type(scope, &ty).is_empty() {
            return None;
        }
    }
    if let Some(query) = attr(scope, source, "media").map(|q| q.trim().to_string()).filter(|q| !q.is_empty()) {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let ctx = crate::core::dom::geometry_queries::media_context(&st);
        if !page::style::media::MediaQueryList::parse(&query, None).matches(&ctx, &[]) {
            return None;
        }
    }
    Some(url)
}

fn wait_for_source(scope: &mut v8::PinScope, id: u32) {
    with_media(scope, id, |m| {
        m.load = Load::WaitingForSource;
        m.network = NETWORK_NO_SOURCE;
        m.fetching = None;
    });
}

/// `LoadResource`: `currentSrc` is the URL from here on; a MediaSource object URL is attached (which
/// nothing here does), `preload=none` defers the fetch, and anything else is fetched.
fn load_resource(scope: &mut v8::PinScope, id: u32, url: String, load: Load) {
    with_media(scope, id, |m| {
        m.current_src = url.clone();
        m.load = load;
    });
    let names_media_source = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        url.get(..5).is_some_and(|s| s.eq_ignore_ascii_case("blob:")) && crate::modules::url::url::blob_url_names_non_blob(&st, &url)
    };
    if names_media_source {
        // the MediaSource the URL names is attached, and opens; nothing is fetched
        let object = crate::modules::url::url::blob_url_object(scope, &url);
        let source = object.and_then(|o| crate::core::media::mse::source_of(scope, o));
        match source {
            Some(source) if crate::core::media::mse::attach(scope, source, id) => {
                with_media(scope, id, |m| m.mse = Some(source));
            }
            _ => loading_failed(scope, id, FORMAT_ERROR),
        }
        return;
    }
    let preload_none = attr(scope, id, "preload").is_some_and(|p| p.trim().eq_ignore_ascii_case("none"));
    if preload_none && attr(scope, id, "autoplay").is_none() {
        with_media(scope, id, |m| {
            m.load = Load::Deferred(url);
            m.network = NETWORK_IDLE;
        });
        queue(scope, id, Task::Fire("progress"));
        queue(scope, id, Task::Fire("suspend"));
        return;
    }
    fetch(scope, id, url);
}

fn fetch(scope: &mut v8::PinScope, id: u32, url: String) {
    let generation = with_media(scope, id, |m| {
        m.fetching = Some(url.clone());
        m.network = NETWORK_LOADING;
        m.generation
    });
    let video = is_video(scope, id);
    let destination = if video { "video" } else { "audio" };
    let cors = attr(scope, id, "crossorigin");
    let mode = if cors.is_some() { "cors" } else { "no-cors" };
    // `media-src`: a resource the document's CSP refuses is never fetched and fails as an
    // unloadable one does; each refusing policy reports a violation.
    if crate::core::html::csp::element_request_blocked(scope, id, &url, destination) {
        loading_failed(scope, id, FORMAT_ERROR);
        return;
    }
    crate::core::timing::resource_timing::note_initiator(scope, id, destination);
    // `crossorigin=use-credentials` includes credentials on the CORS request.
    let credentials = if cors.as_deref().is_some_and(|v| v.trim().eq_ignore_ascii_case("use-credentials")) { "include" } else { "" };
    crate::core::html::subresource::set_pending_load_shape(scope, crate::core::html::subresource::LoadShape { priority: None, credentials, parent: None });
    let Some(p) = crate::core::html::subresource::start_fetch(scope, &url, destination, mode) else {
        loading_failed(scope, id, FORMAT_ERROR);
        return;
    };
    let data = v8::Array::new(scope, 3);
    let i = v8::Number::new(scope, id as f64);
    data.set_index(scope, 0, i.into());
    let g = v8::Number::new(scope, generation as f64);
    data.set_index(scope, 1, g.into());
    let u = str_val(scope, &url);
    data.set_index(scope, 2, u);
    let ok = build_fn_data(scope, cb_fetch_ok, "", 1, data.into());
    let err = build_fn_data(scope, cb_fetch_err, "", 1, data.into());
    // the fetch's end is a task, not a promise job (`then_task`)
    crate::then_task(scope, p, ok, Some(err));
}

/// The completion's element, if the fetch is still the one it waits for: a later load (a new `src`,
/// `load()`) supersedes it, and Chrome aborts the fetch rather than reporting it.
fn current_fetch(scope: &mut v8::PinScope, data: v8::Local<v8::Value>) -> Option<(u32, String)> {
    let arr = v8::Local::<v8::Array>::try_from(data).ok()?;
    let id = arr.get_index(scope, 0)?.uint32_value(scope)?;
    let generation = arr.get_index(scope, 1)?.number_value(scope)? as u64;
    let url = arr.get_index(scope, 2)?.to_rust_string_lossy(scope);
    let live = read(scope, id, |m| m.generation == generation && m.fetching.as_deref() == Some(url.as_str())).unwrap_or(false);
    live.then_some((id, url))
}

fn cb_fetch_ok(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some((id, url)) = current_fetch(scope, __body_data) else { return };
    let status = v8::Local::<v8::Object>::try_from(args.get(0))
        .ok()
        .and_then(|r| get_prop(scope, r, "status"))
        .and_then(|s| s.int32_value(scope))
        .unwrap_or(0);
    with_media(scope, id, |m| m.fetching = None);
    if !(200..300).contains(&status) {
        loading_failed(scope, id, FORMAT_ERROR);
        return;
    }
    // The whole body is here: the network goes idle (`progress`, `suspend`), then the header decides.
    with_media(scope, id, |m| m.network = NETWORK_IDLE);
    queue(scope, id, Task::Fire("progress"));
    queue(scope, id, Task::Fire("suspend"));
    let probe = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<MediaProbes>()).and_then(|p| p.get(&url))
    };
    match probe.flatten() {
        Some(p) => {
            metadata(scope, id, p);
            let body = {
                let op_state = deno_core::JsRuntime::op_state_from(scope);
                let st = op_state.borrow();
                st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<MediaProbes>()).and_then(|p| p.take_body(&url))
            };
            if let Some(body) = body {
                start_pipe(scope, id, body);
            }
        }
        None => loading_failed(scope, id, DEMUXER_ERROR),
    }
}

/// The render backend, which makes the decoders.
fn render_backend(scope: &mut v8::PinScope) -> Option<std::sync::Arc<dyn spec::RenderBackend>> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::RenderHandle>()).map(|h| h.0.clone())
}

/// Element `id`'s pictures: its resource's first video track, decoded on a thread of its own from here on. A
/// format the backend has no decoder for leaves the element showing its poster.
fn start_pipe(scope: &mut v8::PinScope, id: u32, body: std::sync::Arc<Vec<u8>>) {
    use crate::core::media::{demux, video_pipe};
    let Some(backend) = render_backend(scope) else { return };
    let Some((init, samples)) = demux::movie(body) else { return };
    let Some(track) = init.tracks.iter().find(|t| t.kind == demux::Kind::Video) else { return };
    if backend.video_decoder(&track.codec).is_none() {
        return;
    }
    let codec = std::sync::Arc::new(track.codec.clone());
    let coded: Vec<video_pipe::Coded> = samples
        .into_iter()
        .filter(|s| s.track == track.id)
        .filter_map(|s| Some(video_pipe::Coded { dts: s.dts, pts: s.pts, duration: s.duration, key: s.key, data: s.data?, codec: codec.clone() }))
        .collect();
    if coded.is_empty() {
        return;
    }
    let pipe = video_pipe::Pipe::new(backend, true);
    pipe.add(coded);
    with_media(scope, id, |m| {
        pipe.set_clock(clock_of(m));
        m.pipe = Some(pipe);
    });
}

fn clock_of(m: &Media) -> crate::core::media::video_pipe::Clock {
    crate::core::media::video_pipe::Clock { position: m.position, since: m.since }
}

/// Hand element `id`'s clock to its decoding thread, after the clock started, stopped or moved.
fn sync_clock(scope: &mut v8::PinScope, id: u32) {
    read(scope, id, |m| {
        if let Some(p) = &m.pipe {
            p.set_clock(clock_of(m));
        }
    });
}

/// The picture video element `id` shows now, if it decodes one.
pub(crate) fn video_frame(scope: &mut v8::PinScope, id: u32) -> Option<std::sync::Arc<spec::Rgba8>> {
    read(scope, id, |m| m.pipe.as_ref().and_then(|p| p.frame())).flatten()
}

/// Are element `id`'s pictures origin-clean -- its resource same-origin with the document, a `data:` or `blob:` URL, or
/// fetched under CORS (`crossorigin`, which a refused response fails)? Drawing one that is not taints a canvas.
pub(crate) fn origin_clean(scope: &mut v8::PinScope, id: u32) -> bool {
    let Some(src) = read(scope, id, |m| m.current_src.clone()) else { return true };
    if attr(scope, id, "crossorigin").is_some() {
        return true;
    }
    let Ok(u) = url::Url::parse(&src) else { return true };
    if matches!(u.scheme(), "data" | "blob") {
        return true;
    }
    let base = crate::core::html::subresource::node_base_url(scope, id);
    url::Url::parse(&base).is_ok_and(|b| b.origin() == u.origin())
}

/// Does element `id` still show its poster rather than a frame (HTML's show poster flag)?
pub(crate) fn shows_poster(scope: &mut v8::PinScope, id: u32) -> bool {
    read(scope, id, |m| m.show_poster).unwrap_or(true)
}

/// Is a video with decoded pictures playing? Its picture changes at its frame rate, and the page paints at the
/// display's while one does.
pub(crate) fn video_playing(scope: &mut v8::PinScope) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<MediaElements>().is_some_and(|all| all.map.values().any(|m| m.pipe.is_some() && m.since.is_some()))
}

fn cb_fetch_err(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some((id, _)) = current_fetch(scope, __body_data) else { return };
    with_media(scope, id, |m| m.fetching = None);
    loading_failed(scope, id, FORMAT_ERROR);
}

/// `MediaLoadingFailed` before metadata: a `<source>` candidate is passed over (its `error`, then the
/// next); a `src` resource is the dedicated media source failure steps.
fn loading_failed(scope: &mut v8::PinScope, id: u32, message: &str) {
    let load = with_media(scope, id, |m| m.load.clone());
    if let Load::FromSource(source) = load {
        queue(scope, id, Task::FireAtSource(source));
        let more = with_media(scope, id, |m| m.next_source.is_some());
        if more {
            queue(scope, id, Task::NextSource);
        } else {
            wait_for_source(scope, id);
        }
        return;
    }
    none_supported(scope, id, message);
}

/// The dedicated media source failure steps (Blink `NoneSupported`).
fn none_supported(scope: &mut v8::PinScope, id: u32, message: &str) {
    let promises = with_media(scope, id, |m| {
        m.load = Load::WaitingForSource;
        m.fetching = None;
        m.error = Some((4, message.to_string()));
        m.error_obj = None;
        m.network = NETWORK_NO_SOURCE;
        std::mem::take(&mut m.pending_play)
    });
    queue(scope, id, Task::Fire("error"));
    queue(scope, id, Task::Reject(promises, "NotSupportedError", NO_SUPPORTED_SOURCE));
    queue(scope, id, Task::PauseAfterError);
}

/// The header read: HAVE_METADATA, and -- the whole resource being here -- HAVE_ENOUGH_DATA.
fn metadata(scope: &mut v8::PinScope, id: u32, p: MediaProbe) {
    with_media(scope, id, |m| {
        m.duration = p.duration;
        m.size = (p.width, p.height);
        m.position = 0.0;
    });
    set_ready(scope, id, HAVE_METADATA);
    set_ready(scope, id, HAVE_ENOUGH_DATA);
    sync_clock(scope, id);
}

/// fake-media SESSION 2b-events: a plugin-fed `MediaStream` reaching a `<video>` (the cssom paint arm calls this on
/// the first fed frame for an element below HAVE_METADATA). It is [`metadata`] for a live stream: create-or-get the
/// element's `Media` entry (a `srcObject` `<video>` has none), set its size to the fed frame's and its duration to
/// `INFINITY` (a live stream -- which also disarms the end-of-resource clock in [`start_clock`], so no spurious
/// pause/ended), then drive readyState NOTHING -> METADATA -> ENOUGH_DATA through the SHIPPED [`set_ready`] table,
/// which queues the getUserMedia media events (durationchange, resize [video], loadedmetadata, loadeddata, canplay,
/// canplaythrough; and play/playing for an autoplay+muted preview). No new event wiring, and no C ABI (stays 3).
/// `networkState` is deliberately left at NETWORK_EMPTY (byte-identical to today's networkState getter); its real
/// live-stream value is a separate, measured unit.
pub(crate) fn fed_metadata(scope: &mut v8::PinScope, id: u32, w: u32, h: u32) {
    with_media(scope, id, |m| {
        m.duration = f64::INFINITY;
        m.size = (w, h);
        m.position = 0.0;
    });
    set_ready(scope, id, HAVE_METADATA);
    set_ready(scope, id, HAVE_ENOUGH_DATA);
}

/// Blink `SetReadyState`'s event table.
fn set_ready(scope: &mut v8::PinScope, id: u32, new: u8) {
    let old = with_media(scope, id, |m| std::mem::replace(&mut m.ready, new));
    // a playing element that ran out of data stops where it is and waits for more
    if old >= HAVE_FUTURE_DATA && new <= HAVE_CURRENT_DATA && read(scope, id, |m| !m.paused).unwrap_or(false) {
        stop_clock(scope, id);
        queue(scope, id, Task::Fire("timeupdate"));
        queue(scope, id, Task::Fire("waiting"));
        return;
    }
    if new >= HAVE_METADATA && old < HAVE_METADATA {
        queue(scope, id, Task::Fire("durationchange"));
        if is_video(scope, id) {
            queue(scope, id, Task::Fire("resize"));
        }
        queue(scope, id, Task::Fire("loadedmetadata"));
    }
    if new >= HAVE_CURRENT_DATA && old < HAVE_CURRENT_DATA {
        queue(scope, id, Task::Fire("loadeddata"));
    }
    let playing = read(scope, id, |m| !m.paused && m.error.is_none()).unwrap_or(false);
    if new == HAVE_FUTURE_DATA && old <= HAVE_CURRENT_DATA {
        queue(scope, id, Task::Fire("canplay"));
        if playing {
            notify_playing(scope, id);
        }
    }
    if new == HAVE_ENOUGH_DATA && old < HAVE_ENOUGH_DATA {
        if old <= HAVE_CURRENT_DATA {
            queue(scope, id, Task::Fire("canplay"));
            if playing {
                notify_playing(scope, id);
            }
        }
        let should_autoplay = read(scope, id, |m| m.can_autoplay && m.paused).unwrap_or(false) && attr(scope, id, "autoplay").is_some();
        if should_autoplay && autoplay_allowed(scope, id) {
            with_media(scope, id, |m| {
                m.paused = false;
                m.can_autoplay = false;
                m.show_poster = false;
            });
            queue(scope, id, Task::Fire("play"));
            notify_playing(scope, id);
        }
        queue(scope, id, Task::Fire("canplaythrough"));
    }
}

fn notify_playing(scope: &mut v8::PinScope, id: u32) {
    let promises = with_media(scope, id, |m| std::mem::take(&mut m.pending_play));
    queue(scope, id, Task::NotifyPlaying(promises));
}

// ---- playback -------------------------------------------------------------------------------------

/// The current playback position.
pub(crate) fn position(m: &Media) -> f64 {
    match m.since {
        Some(t) => {
            let p = m.position + t.elapsed().as_secs_f64();
            if m.duration.is_finite() { p.min(m.duration) } else { p }
        }
        None => m.position,
    }
}

/// How far the playback clock has covered the resource -- `played`'s one range.
pub(crate) fn played_end(m: &Media) -> f64 {
    m.played_to.max(if m.since.is_some() { position(m) } else { 0.0 })
}

/// Has the element ended playback (forwards, no `loop`)? The `loop` test is the caller's.
pub(crate) fn at_end(m: &Media) -> bool {
    m.ready >= HAVE_METADATA && m.duration.is_finite() && position(m) >= m.duration
}

fn start_clock(scope: &mut v8::PinScope, id: u32) {
    let (session, remaining) = with_media(scope, id, |m| {
        if m.paused || m.since.is_some() {
            return (None, 0.0);
        }
        m.since = Some(std::time::Instant::now());
        m.session += 1;
        (Some(m.session), m.duration - m.position)
    });
    if let Some(s) = session {
        if remaining.is_finite() {
            queue_after(scope, id, Task::ReachEnd(s), (remaining.max(0.0) * 1000.0).ceil());
        }
        queue_after(scope, id, Task::Tick(s), TIMEUPDATE_MS);
        arm_stall(scope, id, s);
    }
    sync_clock(scope, id);
}

/// An MSE element's buffered ranges and whether its source has ended (`None` for any other element).
fn mse_ranges(scope: &mut v8::PinScope, id: u32) -> Option<(Vec<(f64, f64)>, bool)> {
    let source = read(scope, id, |m| m.mse).flatten()?;
    let ranges = crate::core::media::mse::element_buffered(scope, source);
    let ended = crate::core::media::mse::source_state(scope, source) == crate::core::media::mse::ReadyState::Ended;
    Some((ranges, ended))
}

/// Where the buffered range holding `pos` ends (`pos` itself when none holds it). A frame's worth of slack at its
/// start: a range that begins a few milliseconds after the position still plays from it.
fn range_end(ranges: &[(f64, f64)], pos: f64) -> f64 {
    ranges.iter().find(|(s, e)| pos >= *s - 0.1 && pos < *e).map_or(pos, |r| r.1)
}

/// Arm the stall check of a playing MSE element: when its clock reaches the end of the frames buffered.
fn arm_stall(scope: &mut v8::PinScope, id: u32, session: u64) {
    let Some((ranges, ended)) = mse_ranges(scope, id) else { return };
    if ended {
        return;
    }
    let pos = read(scope, id, position).unwrap_or(0.0);
    let end = range_end(&ranges, pos);
    queue_after(scope, id, Task::Stall(session), ((end - pos).max(0.0) * 1000.0).ceil());
}

/// The stall check: still short of the buffered end (more came) arms it again; at it, the element stops there and
/// waits (HAVE_METADATA: `timeupdate`, `waiting`). A source that ended plays on to its end instead.
fn stall(scope: &mut v8::PinScope, id: u32, session: u64) {
    if !read(scope, id, |m| m.session == session && m.since.is_some()).unwrap_or(false) {
        return;
    }
    let Some((ranges, ended)) = mse_ranges(scope, id) else { return };
    if ended {
        return;
    }
    let pos = read(scope, id, position).unwrap_or(0.0);
    let end = range_end(&ranges, pos);
    if pos + 0.005 < end {
        queue_after(scope, id, Task::Stall(session), ((end - pos) * 1000.0).ceil());
        return;
    }
    with_media(scope, id, |m| {
        m.position = end.min(position(m));
        m.since = None;
        m.session += 1;
    });
    set_ready(scope, id, HAVE_METADATA);
    sync_clock(scope, id);
}

/// The readiness an MSE element's buffered frames give it at its position: HAVE_ENOUGH_DATA when a range holds it (or
/// the stream ended and it is at the end), HAVE_METADATA otherwise -- then the events of the change.
pub(crate) fn mse_update(scope: &mut v8::PinScope, id: u32) {
    let Some((ranges, ended)) = mse_ranges(scope, id) else { return };
    let (ready, pos) = match read(scope, id, |m| (m.ready, position(m))) {
        Some(v) => v,
        None => return,
    };
    if ready < HAVE_METADATA {
        return;
    }
    let held = range_end(&ranges, pos) > pos;
    let at_end = ended && ranges.last().is_some_and(|r| pos >= r.1 - 0.005);
    let new = if held || at_end { HAVE_ENOUGH_DATA } else { HAVE_METADATA };
    if new != ready {
        set_ready(scope, id, new);
    }
    // a clock already running has more to play: its stall check moves out
    let session = read(scope, id, |m| m.since.map(|_| m.session)).flatten();
    if let Some(s) = session {
        arm_stall(scope, id, s);
    }
}

/// An MSE element's first initialization segments are all in: its size, the decoder for its video track when the
/// backend has one, and -- a task later, as Chrome's pipeline reports it once it has started, by when the append's
/// frames have set the duration -- HAVE_METADATA (Chrome 150: `readyState` is still 0 at the first `updateend`, and one
/// `durationchange` carries the duration the frames reached).
pub(crate) fn mse_metadata(scope: &mut v8::PinScope, id: u32, duration: f64, size: (u32, u32), video: Option<std::sync::Arc<spec::VideoCodec>>) {
    with_media(scope, id, |m| {
        m.duration = duration;
        m.size = size;
    });
    queue(scope, id, Task::MseMetadata);
    let has_pipe = read(scope, id, |m| m.pipe.is_some()).unwrap_or(false);
    if let (false, Some(codec), Some(backend)) = (has_pipe, video, render_backend(scope)) {
        if backend.video_decoder(&codec).is_some() {
            let pipe = crate::core::media::video_pipe::Pipe::new(backend, false);
            with_media(scope, id, |m| {
                pipe.set_clock(clock_of(m));
                m.pipe = Some(pipe);
            });
        }
    }
}

/// The source's duration changed (`durationchange`).
pub(crate) fn mse_duration(scope: &mut v8::PinScope, id: u32, d: f64) {
    let changed = with_mse_duration(scope, id, d);
    if changed && read(scope, id, |m| m.ready).unwrap_or(HAVE_NOTHING) >= HAVE_METADATA {
        queue(scope, id, Task::Fire("durationchange"));
    }
}

fn with_mse_duration(scope: &mut v8::PinScope, id: u32, d: f64) -> bool {
    with_media(scope, id, |m| {
        let changed = m.duration != d && !(m.duration.is_nan() && d.is_nan());
        m.duration = d;
        if m.position > d {
            m.position = d;
        }
        changed
    })
}

/// Coded video frames appended to an MSE element's source, for its decoder.
pub(crate) fn mse_frames(scope: &mut v8::PinScope, id: u32, frames: Vec<crate::core::media::video_pipe::Coded>) {
    read(scope, id, |m| {
        if let Some(p) = &m.pipe {
            p.add(frames);
        }
    });
}

/// Frames removed from an MSE element's source.
pub(crate) fn mse_removed(scope: &mut v8::PinScope, id: u32, start: f64, end: f64) {
    read(scope, id, |m| {
        if let Some(p) = &m.pipe {
            p.remove(start, end);
        }
    });
}

/// `endOfStream("network" | "decode")`: before metadata the source is not supported; after, the element fails with
/// a network or decode error.
pub(crate) fn mse_error(scope: &mut v8::PinScope, id: u32, network: bool) {
    let ready = read(scope, id, |m| m.ready).unwrap_or(HAVE_NOTHING);
    if ready == HAVE_NOTHING {
        none_supported(scope, id, FORMAT_ERROR);
        return;
    }
    let code = if network { 2 } else { 3 };
    let message = if network { "PIPELINE_ERROR_NETWORK" } else { "PIPELINE_ERROR_DECODE" };
    with_media(scope, id, |m| {
        m.error = Some((code, message.to_string()));
        m.error_obj = None;
        m.network = NETWORK_IDLE;
    });
    queue(scope, id, Task::Fire("error"));
}

/// An MSE element's `buffered` and `seekable`: `None` for any other element.
pub(crate) fn mse_buffered(scope: &mut v8::PinScope, id: u32, which: &str) -> Option<Vec<(f64, f64)>> {
    let (ranges, _) = mse_ranges(scope, id)?;
    if which == "buffered" {
        return Some(ranges);
    }
    let duration = read(scope, id, |m| m.duration).unwrap_or(f64::NAN);
    Some(if duration.is_nan() {
        Vec::new()
    } else if duration.is_infinite() {
        ranges.first().zip(ranges.last()).map(|(a, b)| vec![(a.0, b.1)]).unwrap_or_default()
    } else {
        vec![(0.0, duration)]
    })
}

fn stop_clock(scope: &mut v8::PinScope, id: u32) {
    with_media(scope, id, |m| {
        let p = position(m);
        m.position = p;
        m.played_to = m.played_to.max(p);
        m.since = None;
        m.session += 1;
    });
    sync_clock(scope, id);
}

/// The playback clock reached the end: `timeupdate`, then -- forwards, no `loop`, still playing --
/// `pause`, and `ended`. With `loop` the clock starts over.
fn reach_end(scope: &mut v8::PinScope, id: u32, session: u64) {
    let current = read(scope, id, |m| m.session == session && !m.paused).unwrap_or(false);
    if !current {
        return;
    }
    if attr(scope, id, "loop").is_some() {
        with_media(scope, id, |m| {
            m.played_to = m.duration;
            m.position = 0.0;
            m.since = None;
        });
        fire(scope, id, "seeking");
        fire(scope, id, "timeupdate");
        fire(scope, id, "seeked");
        start_clock(scope, id);
        return;
    }
    let promises = with_media(scope, id, |m| {
        m.position = m.duration;
        m.played_to = m.duration;
        m.since = None;
        m.session += 1;
        m.paused = true;
        std::mem::take(&mut m.pending_play)
    });
    sync_clock(scope, id);
    fire(scope, id, "timeupdate");
    fire(scope, id, "pause");
    settle(scope, promises, Some(("AbortError", INTERRUPTED_BY_PAUSE)));
    fire(scope, id, "ended");
}

/// `play()` (Blink `playForBindings` + `Play` + `PlayInternal`).
pub(crate) fn play<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32) -> Option<v8::Local<'s, v8::Promise>> {
    let resolver = v8::PromiseResolver::new(scope)?;
    let promise = resolver.get_promise(scope);
    let paused = read(scope, id, |m| m.paused).unwrap_or(true);
    if paused && !autoplay_allowed(scope, id) {
        let e = make_dom_exc(scope, NOT_ALLOWED, "NotAllowedError")?;
        resolver.reject(scope, e);
        return Some(promise);
    }
    if read(scope, id, |m| m.error.as_ref().is_some_and(|e| e.0 == 4)).unwrap_or(false) {
        let e = make_dom_exc(scope, NO_SUPPORTED_SOURCES_SYNC, "NotSupportedError")?;
        resolver.reject(scope, e);
        return Some(promise);
    }
    let g = v8::Global::new(scope, resolver);
    with_media(scope, id, |m| m.pending_play.push(g));
    play_internal(scope, id);
    Some(promise)
}

fn play_internal(scope: &mut v8::PinScope, id: u32) {
    if with_media(scope, id, |m| m.network) == NETWORK_EMPTY {
        resource_selection(scope, id);
    }
    // An element that ended plays from the start (Blink ignores `loop` here).
    with_media(scope, id, |m| {
        if at_end(m) {
            m.position = 0.0;
        }
    });
    let (was_paused, ready) = with_media(scope, id, |m| (std::mem::replace(&mut m.paused, false), m.ready));
    if was_paused {
        with_media(scope, id, |m| m.show_poster = false);
        queue(scope, id, Task::Fire("play"));
        if ready <= HAVE_CURRENT_DATA {
            queue(scope, id, Task::Fire("waiting"));
        } else {
            notify_playing(scope, id);
        }
    } else if ready >= HAVE_FUTURE_DATA {
        let promises = with_media(scope, id, |m| std::mem::take(&mut m.pending_play));
        queue(scope, id, Task::Resolve(promises));
    }
    let deferred = with_media(scope, id, |m| {
        m.can_autoplay = false;
        match std::mem::take(&mut m.load) {
            Load::Deferred(url) => Some(url),
            other => {
                m.load = other;
                None
            }
        }
    });
    if let Some(url) = deferred {
        with_media(scope, id, |m| m.load = Load::FromAttribute);
        fetch(scope, id, url);
    }
}

/// `pause()` (Blink `PauseInternal`).
pub(crate) fn pause(scope: &mut v8::PinScope, id: u32) {
    if with_media(scope, id, |m| m.network) == NETWORK_EMPTY {
        resource_selection(scope, id);
    }
    let was_playing = with_media(scope, id, |m| {
        m.can_autoplay = false;
        !std::mem::replace(&mut m.paused, true)
    });
    if was_playing {
        stop_clock(scope, id);
        let promises = with_media(scope, id, |m| std::mem::take(&mut m.pending_play));
        queue(scope, id, Task::Fire("timeupdate"));
        queue(scope, id, Task::Fire("pause"));
        queue(scope, id, Task::Reject(promises, "AbortError", INTERRUPTED_BY_PAUSE));
    }
}

/// `currentTime = t`: with metadata the element seeks (`seeking`, `timeupdate`, `seeked`); without,
/// the value is the default playback start position the getter reports. Returns whether the element's
/// state took it.
pub(crate) fn seek(scope: &mut v8::PinScope, id: u32, t: f64) -> bool {
    let Some(ready) = read(scope, id, |m| m.ready) else { return false };
    if ready < HAVE_METADATA {
        return false;
    }
    let restart = with_media(scope, id, |m| {
        let t = if m.duration.is_finite() { t.clamp(0.0, m.duration) } else { t.max(0.0) };
        let playing = m.since.is_some();
        m.played_to = m.played_to.max(position(m));
        m.position = t;
        m.since = None;
        m.session += 1;
        m.show_poster = false;
        playing
    });
    sync_clock(scope, id);
    mse_update(scope, id);
    queue(scope, id, Task::Fire("seeking"));
    queue(scope, id, Task::Fire("timeupdate"));
    queue(scope, id, Task::Fire("seeked"));
    if restart {
        start_clock(scope, id);
    }
    true
}

/// The `MediaError` `error` hands out, minted once per failure.
pub(crate) fn error_object<'s>(scope: &mut v8::PinScope<'s, '_>, id: u32) -> Option<v8::Local<'s, v8::Object>> {
    let (code, message, cached) = read(scope, id, |m| m.error.clone().map(|(c, s)| (c, s, m.error_obj.clone())))??;
    if let Some(g) = cached {
        return Some(v8::Local::new(scope, &g));
    }
    let o = crate::core::html::media_objects::mint_media_error(scope, code, &message)?;
    let g = v8::Global::new(scope, o);
    with_media(scope, id, |m| m.error_obj = Some(g));
    Some(o)
}

/// The element a getter or method was called on, if it is a node.
pub(crate) fn this_id(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Option<u32> {
    unwrap_id(scope, this.into())
}
