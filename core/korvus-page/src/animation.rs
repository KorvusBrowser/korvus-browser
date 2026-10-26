//! `animation` — the Web Animations timing and playback model, as pure arithmetic.
//!
//! # Why this lives in `korvus-page`
//!
//! An animation's observable state is a *function* of `(timeline time, start time, hold time,
//! playback rate, timing, keyframes)`. Nothing in this file rasterises, composites, or touches V8:
//! `currentTime`, `playState`, `getComputedTiming()` and the value a keyframe interpolates to are
//! all arithmetic over that tuple. That is why the model belongs in the crate that already owns
//! computation over the document, and why `scripts/check-invariants.sh` keeping this crate free of
//! `deno_core`/`v8`/skia costs the timeline nothing.
//!
//! The V8 half — the `Animation` / `KeyframeEffect` / `DocumentTimeline` objects a page holds, the
//! promises, the events — is `core/korvus-webapi/src/core/animation/`. It owns the *identity* of
//! an animation; this file owns its *numbers*.
//!
//! # What is modelled
//!
//! * [`Timing`] — the WebIDL `EffectTiming` dictionary after normalisation, plus the spec's derived
//!   quantities (`activeDuration`, `endTime`).
//! * [`Easing`] — `linear`, the five CSS keywords, `cubic-bezier()` and `steps()`, evaluated the way
//!   CSS Easing Functions Level 1 defines them.
//! * [`ComputedTiming`] — the result of the spec's local-time → phase → active time → iteration
//!   progress → directed progress → transformed progress pipeline, which is exactly what
//!   `AnimationEffect.getComputedTiming()` reports.
//! * [`Playback`] — the animation's own state machine: `play`, `pause`, `finish`, `cancel`,
//!   `reverse`, `updatePlaybackRate`, and the `playState` those produce.
//!
//! # What is deliberately NOT modelled
//!
//! There is no pending-task queue. A pending play/pause task exists in the spec so that an
//! implementation can defer starting an animation until the compositor has a frame; this engine has
//! no compositor and no vsync (`requestAnimationFrame` is a 16 ms timer over `performance.now()`),
//! so every playback change applies synchronously and `Animation.pending` is therefore always
//! `false`. That is the honest answer for this engine rather than a simplification: there is no
//! task to be pending on.

/// The `FillMode` enum. `Auto` is what the WebIDL default resolves *from*; for a `KeyframeEffect`
/// it resolves to [`FillMode::None`], which is the only resolution this engine needs (CSS
//  animations, which resolve `auto` from the `animation-fill-mode` property, are not created here).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillMode {
    None,
    Forwards,
    Backwards,
    Both,
    Auto,
}

impl FillMode {
    pub fn parse(s: &str) -> Option<FillMode> {
        Some(match s {
            "none" => FillMode::None,
            "forwards" => FillMode::Forwards,
            "backwards" => FillMode::Backwards,
            "both" => FillMode::Both,
            "auto" => FillMode::Auto,
            _ => return None,
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            FillMode::None => "none",
            FillMode::Forwards => "forwards",
            FillMode::Backwards => "backwards",
            FillMode::Both => "both",
            FillMode::Auto => "auto",
        }
    }
    /// `auto` on a `KeyframeEffect` resolves to `none` (Web Animations §"fill mode").
    pub fn resolved(self) -> FillMode {
        match self {
            FillMode::Auto => FillMode::None,
            other => other,
        }
    }
    fn fills_backwards(self) -> bool {
        matches!(self.resolved(), FillMode::Backwards | FillMode::Both)
    }
    fn fills_forwards(self) -> bool {
        matches!(self.resolved(), FillMode::Forwards | FillMode::Both)
    }
}

/// The `PlaybackDirection` enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackDirection {
    Normal,
    Reverse,
    Alternate,
    AlternateReverse,
}

impl PlaybackDirection {
    pub fn parse(s: &str) -> Option<PlaybackDirection> {
        Some(match s {
            "normal" => PlaybackDirection::Normal,
            "reverse" => PlaybackDirection::Reverse,
            "alternate" => PlaybackDirection::Alternate,
            "alternate-reverse" => PlaybackDirection::AlternateReverse,
            _ => return None,
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            PlaybackDirection::Normal => "normal",
            PlaybackDirection::Reverse => "reverse",
            PlaybackDirection::Alternate => "alternate",
            PlaybackDirection::AlternateReverse => "alternate-reverse",
        }
    }
}

/// Where a `steps()` easing jumps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepPosition {
    Start,
    End,
    JumpNone,
    JumpBoth,
}

/// A parsed CSS easing function. Kept as a value (not a closure) so it round-trips through
/// `getTiming()` unchanged — a page that sets `easing: "ease-in-out"` must read back the string it
/// wrote, not a normalised `cubic-bezier(...)`.
#[derive(Clone, Debug, PartialEq)]
pub enum Easing {
    Linear,
    CubicBezier(f64, f64, f64, f64),
    Steps(u32, StepPosition),
}

impl Easing {
    /// Parse a CSS `<easing-function>`. `None` means the string is not a valid easing, which the
    /// WebIDL layer turns into a `TypeError` (Web Animations: "If the easing is not a valid
    /// `<easing-function>` value, throw a TypeError").
    pub fn parse(input: &str) -> Option<Easing> {
        let s = input.trim();
        match s {
            "linear" => return Some(Easing::Linear),
            "ease" => return Some(Easing::CubicBezier(0.25, 0.1, 0.25, 1.0)),
            "ease-in" => return Some(Easing::CubicBezier(0.42, 0.0, 1.0, 1.0)),
            "ease-out" => return Some(Easing::CubicBezier(0.0, 0.0, 0.58, 1.0)),
            "ease-in-out" => return Some(Easing::CubicBezier(0.42, 0.0, 0.58, 1.0)),
            "step-start" => return Some(Easing::Steps(1, StepPosition::Start)),
            "step-end" => return Some(Easing::Steps(1, StepPosition::End)),
            _ => {}
        }
        if let Some(args) = fn_args(s, "cubic-bezier") {
            let n: Vec<f64> = args.iter().filter_map(|a| a.trim().parse::<f64>().ok()).collect();
            if n.len() != 4 || args.len() != 4 {
                return None;
            }
            // The x coordinates are the input progress, which is clamped to [0,1]; a control point
            // outside that range would make the curve non-monotonic in x and unsolvable.
            if !(0.0..=1.0).contains(&n[0]) || !(0.0..=1.0).contains(&n[2]) {
                return None;
            }
            if n.iter().any(|v| !v.is_finite()) {
                return None;
            }
            return Some(Easing::CubicBezier(n[0], n[1], n[2], n[3]));
        }
        if let Some(args) = fn_args(s, "steps") {
            if args.is_empty() || args.len() > 2 {
                return None;
            }
            let count: i64 = args[0].trim().parse().ok()?;
            let pos = match args.get(1).map(|a| a.trim()) {
                None | Some("end") | Some("jump-end") => StepPosition::End,
                Some("start") | Some("jump-start") => StepPosition::Start,
                Some("jump-none") => StepPosition::JumpNone,
                Some("jump-both") => StepPosition::JumpBoth,
                Some(_) => return None,
            };
            // `jump-none` needs at least two steps to have anywhere to jump between; every other
            // form needs at least one.
            let min = if pos == StepPosition::JumpNone { 2 } else { 1 };
            if count < min {
                return None;
            }
            return Some(Easing::Steps(count as u32, pos));
        }
        None
    }

    /// Evaluate the easing at input progress `t`. `t` is the *directed* progress and is normally in
    /// `[0,1]`; values outside are extrapolated linearly off the curve's endpoints, which is what
    /// the CSS easing spec asks for before/after the interval.
    pub fn eval(&self, t: f64) -> f64 {
        match *self {
            Easing::Linear => t,
            Easing::CubicBezier(x1, y1, x2, y2) => cubic_bezier_eval(x1, y1, x2, y2, t),
            Easing::Steps(count, pos) => {
                let n = count as f64;
                let mut current = (t * n).floor();
                // `jump-start` and `jump-both` take the step at the START of the interval, so the
                // current step is one higher for the same input.
                if matches!(pos, StepPosition::Start | StepPosition::JumpBoth) {
                    current += 1.0;
                }
                let jumps = match pos {
                    StepPosition::Start | StepPosition::End => n,
                    StepPosition::JumpNone => n - 1.0,
                    StepPosition::JumpBoth => n + 1.0,
                };
                if t >= 0.0 && current < 0.0 {
                    current = 0.0;
                }
                if t <= 1.0 && current > jumps {
                    current = jumps;
                }
                current / jumps
            }
        }
    }
}

/// Split `name(a, b, c)` into its argument strings. `None` when `s` is not a call to `name`.
fn fn_args<'a>(s: &'a str, name: &str) -> Option<Vec<&'a str>> {
    let rest = s.strip_prefix(name)?;
    let rest = rest.trim_start();
    let inner = rest.strip_prefix('(')?.strip_suffix(')')?;
    if inner.trim().is_empty() {
        return Some(Vec::new());
    }
    Some(inner.split(',').collect())
}

/// Solve `x(t) = target` on the unit cubic Bézier with control points `(x1,y1)`/`(x2,y2)` and
/// return `y(t)`. Newton–Raphson with a bisection fallback, the same shape Blink's
/// `gfx::CubicBezier` uses (the algorithm is textbook; only the constants are shared).
fn cubic_bezier_eval(x1: f64, y1: f64, x2: f64, y2: f64, target: f64) -> f64 {
    // Outside [0,1] the curve is extended along its endpoint tangent (CSS Easing §"output range").
    if target <= 0.0 {
        let slope = if x1 > 0.0 {
            y1 / x1
        } else if y1 == 0.0 && x2 > 0.0 {
            y2 / x2
        } else {
            0.0
        };
        return slope * target;
    }
    if target >= 1.0 {
        let slope = if x2 < 1.0 {
            (y2 - 1.0) / (x2 - 1.0)
        } else if y2 == 1.0 && x1 < 1.0 {
            (y1 - 1.0) / (x1 - 1.0)
        } else {
            0.0
        };
        return 1.0 + slope * (target - 1.0);
    }
    let bez = |a: f64, b: f64, t: f64| {
        let mt = 1.0 - t;
        3.0 * mt * mt * t * a + 3.0 * mt * t * t * b + t * t * t
    };
    let bez_deriv = |a: f64, b: f64, t: f64| {
        let mt = 1.0 - t;
        3.0 * mt * mt * a + 6.0 * mt * t * (b - a) + 3.0 * t * t * (1.0 - b)
    };
    let mut t = target;
    for _ in 0..8 {
        let x = bez(x1, x2, t) - target;
        if x.abs() < 1e-7 {
            return bez(y1, y2, t);
        }
        let d = bez_deriv(x1, x2, t);
        if d.abs() < 1e-9 {
            break;
        }
        t -= x / d;
    }
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    let mut t = target;
    for _ in 0..32 {
        let x = bez(x1, x2, t);
        if (x - target).abs() < 1e-7 {
            break;
        }
        if x < target {
            lo = t;
        } else {
            hi = t;
        }
        t = (lo + hi) / 2.0;
    }
    bez(y1, y2, t)
}

/// The `EffectTiming` dictionary after WebIDL conversion, i.e. what `getTiming()` reports.
#[derive(Clone, Debug, PartialEq)]
pub struct Timing {
    pub delay: f64,
    pub end_delay: f64,
    pub fill: FillMode,
    pub iteration_start: f64,
    /// May be `f64::INFINITY`.
    pub iterations: f64,
    /// `None` is the WebIDL `"auto"` duration. For a `KeyframeEffect` `auto` resolves to 0.
    pub duration: Option<f64>,
    pub direction: PlaybackDirection,
    /// The easing string EXACTLY as the page supplied it — `getTiming()` must echo it back.
    pub easing_text: String,
    pub easing: Easing,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            delay: 0.0,
            end_delay: 0.0,
            fill: FillMode::Auto,
            iteration_start: 0.0,
            iterations: 1.0,
            duration: None,
            direction: PlaybackDirection::Normal,
            easing_text: "linear".to_string(),
            easing: Easing::Linear,
        }
    }
}

impl Timing {
    /// The resolved iteration duration: `auto` is 0 for a keyframe effect.
    pub fn iteration_duration(&self) -> f64 {
        self.duration.unwrap_or(0.0)
    }

    /// `active duration = iteration duration × iteration count`, with the spec's `0 × ∞ = 0` rule.
    pub fn active_duration(&self) -> f64 {
        let d = self.iteration_duration();
        if d == 0.0 || self.iterations == 0.0 {
            0.0
        } else {
            d * self.iterations
        }
    }

    /// `end time = max(delay + active duration + end delay, 0)`.
    pub fn end_time(&self) -> f64 {
        let t = self.delay + self.active_duration() + self.end_delay;
        if t.is_nan() || t < 0.0 {
            0.0
        } else {
            t
        }
    }
}

/// Which phase the effect's local time falls in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Before,
    Active,
    After,
    Idle,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Before => "before",
            Phase::Active => "active",
            Phase::After => "after",
            Phase::Idle => "idle",
        }
    }
}

/// What `AnimationEffect.getComputedTiming()` reports: the resolved timing plus the four values the
/// timing model derives from the current local time.
#[derive(Clone, Debug, PartialEq)]
pub struct ComputedTiming {
    pub end_time: f64,
    pub active_duration: f64,
    pub local_time: Option<f64>,
    /// The *transformed* progress (easing applied), `None` outside the active/filled interval.
    pub progress: Option<f64>,
    pub current_iteration: Option<f64>,
    pub phase: Phase,
}

/// Run the spec's timing pipeline for `timing` at effect local time `local_time`.
///
/// `local_time` is `None` for an idle animation, and the whole result is then `None` too — which is
/// exactly what `getComputedTiming()` reports before an animation has ever been played.
pub fn compute_timing(timing: &Timing, local_time: Option<f64>) -> ComputedTiming {
    let active_duration = timing.active_duration();
    let end_time = timing.end_time();
    let Some(t) = local_time else {
        return ComputedTiming {
            end_time,
            active_duration,
            local_time: None,
            progress: None,
            current_iteration: None,
            phase: Phase::Idle,
        };
    };

    let before_end = timing.delay;
    let active_end = timing.delay + active_duration;
    // The phase boundaries are half-open on the *active* side, so a zero-length active interval is
    // "after", not "active" — which is what makes `element.animate(kf)` with no duration finish
    // immediately instead of sitting at progress 0 forever.
    let phase = if t < before_end {
        Phase::Before
    } else if t >= active_end {
        Phase::After
    } else {
        Phase::Active
    };

    // ── active time ──
    let active_time = match phase {
        Phase::Before => timing.fill.fills_backwards().then_some(0.0),
        Phase::Active => Some(t - timing.delay),
        Phase::After => timing.fill.fills_forwards().then_some(active_duration),
        Phase::Idle => None,
    };
    let Some(active_time) = active_time else {
        return ComputedTiming {
            end_time,
            active_duration,
            local_time: Some(t),
            progress: None,
            current_iteration: None,
            phase,
        };
    };

    let iter_dur = timing.iteration_duration();
    // ── overall progress ──
    let overall = if iter_dur == 0.0 {
        // A zero-duration iteration: every iteration is instantaneous, so the progress is the
        // iteration START in the before phase and the full iteration count in the after phase.
        match phase {
            Phase::After => timing.iteration_start + timing.iterations,
            _ => timing.iteration_start,
        }
    } else {
        timing.iteration_start + active_time / iter_dur
    };

    // ── simple iteration progress + current iteration ──
    let (simple, current_iteration) = if !overall.is_finite() {
        // An infinite iteration count that has reached the after phase cannot be reached (the
        // active duration is infinite), so this only happens for iterations == ∞ mid-flight, where
        // `overall` is finite. A non-finite overall means iteration_start was ±∞, which WebIDL
        // rejects; fall back to the start of the first iteration.
        (0.0, 0.0)
    } else {
        let frac = overall % 1.0;
        let mut simple = if frac == 0.0 && phase == Phase::After && timing.iterations != 0.0 {
            1.0
        } else {
            frac
        };
        let mut iteration = if overall == f64::INFINITY {
            f64::INFINITY
        } else if simple == 1.0 {
            (overall.floor() - 1.0).max(0.0)
        } else {
            overall.floor()
        };
        if iteration < 0.0 {
            iteration = 0.0;
        }
        if simple < 0.0 {
            simple = 0.0;
        }
        (simple, iteration)
    };

    // ── directed progress ──
    let reversed = match timing.direction {
        PlaybackDirection::Normal => false,
        PlaybackDirection::Reverse => true,
        PlaybackDirection::Alternate => (current_iteration as i64) % 2 != 0,
        PlaybackDirection::AlternateReverse => (current_iteration as i64) % 2 == 0,
    };
    let directed = if reversed { 1.0 - simple } else { simple };

    // ── transformed progress: the easing, evaluated on the directed progress. A zero-length
    // iteration in the before/after phase has no interval to ease over, so the boundary value is
    // used directly (CSS Easing "before flag").
    let transformed = if iter_dur == 0.0 && phase != Phase::Active {
        directed
    } else {
        timing.easing.eval(directed)
    };

    ComputedTiming {
        end_time,
        active_duration,
        local_time: Some(t),
        progress: Some(transformed),
        current_iteration: Some(current_iteration),
        phase,
    }
}

/// The `AnimationPlayState` enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayState {
    Idle,
    Running,
    Paused,
    Finished,
}

impl PlayState {
    pub fn as_str(self) -> &'static str {
        match self {
            PlayState::Idle => "idle",
            PlayState::Running => "running",
            PlayState::Paused => "paused",
            PlayState::Finished => "finished",
        }
    }
}

/// The `AnimationReplaceState` enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplaceState {
    Active,
    Removed,
    Persisted,
}

impl ReplaceState {
    pub fn as_str(self) -> &'static str {
        match self {
            ReplaceState::Active => "active",
            ReplaceState::Removed => "removed",
            ReplaceState::Persisted => "persisted",
        }
    }
}

/// Why a playback operation could not be performed. The binding turns each into the DOMException
/// the spec names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackError {
    /// `InvalidStateError` — seeking backwards into an infinitely-long effect, or finishing an
    /// animation whose playback rate is 0.
    InvalidState,
}

/// The animation's own state machine. Everything a page can observe about *when* an animation is —
/// as opposed to *what* it animates — is derived from these five fields.
#[derive(Clone, Debug, PartialEq)]
pub struct Playback {
    /// The timeline time at which the animation's current time was zero. `None` while paused or idle.
    pub start_time: Option<f64>,
    /// A pinned current time. Set while paused, or when the animation is finished/seeked.
    pub hold_time: Option<f64>,
    pub playback_rate: f64,
    pub replace_state: ReplaceState,
    /// Set once the `finish` event has been fired and the `finished` promise resolved for the
    /// current finished run, so neither happens twice for one finish.
    pub finish_notified: bool,
}

impl Default for Playback {
    fn default() -> Self {
        Playback {
            start_time: None,
            hold_time: None,
            playback_rate: 1.0,
            replace_state: ReplaceState::Active,
            finish_notified: false,
        }
    }
}

impl Playback {
    /// `Animation.currentTime`. `None` when the animation is idle or the timeline is inactive.
    pub fn current_time(&self, timeline_time: Option<f64>) -> Option<f64> {
        if let Some(h) = self.hold_time {
            return Some(h);
        }
        match (timeline_time, self.start_time) {
            (Some(t), Some(s)) => Some((t - s) * self.playback_rate),
            _ => None,
        }
    }

    /// `Animation.playState`.
    ///
    /// The spec's ordering is idle → paused → finished → running, and it distinguishes paused from
    /// finished by the presence of a *pending pause task*. This engine has no pending tasks, so the
    /// distinguishing fact is the start time: `pause()` clears it and `finish()` sets it. That is
    /// not a shortcut — with no task queue those two facts carry exactly the same information.
    pub fn play_state(&self, timeline_time: Option<f64>, effect_end: f64) -> PlayState {
        if self.hold_time.is_none() && self.start_time.is_none() {
            return PlayState::Idle;
        }
        if self.start_time.is_none() {
            return PlayState::Paused;
        }
        if let Some(c) = self.current_time(timeline_time) {
            if (self.playback_rate > 0.0 && c >= effect_end) || (self.playback_rate < 0.0 && c <= 0.0) {
                return PlayState::Finished;
            }
        }
        PlayState::Running
    }

    /// `Animation.overallProgress` — the effect's progress across ALL iterations, `null` when the
    /// animation is idle or has no resolved current time.
    pub fn overall_progress(&self, timeline_time: Option<f64>, timing: &Timing) -> Option<f64> {
        let t = self.current_time(timeline_time)?;
        let end = timing.end_time();
        if end == 0.0 {
            // A zero-length effect is either not started (0) or done (1).
            return Some(if t >= 0.0 { 1.0 } else { 0.0 });
        }
        if !end.is_finite() {
            return None;
        }
        Some((t / end).clamp(0.0, 1.0))
    }

    /// Seek: `animation.currentTime = value` (and the internal seeks `play`/`finish` perform).
    pub fn set_current_time(&mut self, timeline_time: Option<f64>, seek: f64) {
        if self.hold_time.is_some() || self.start_time.is_none() || timeline_time.is_none() || self.playback_rate == 0.0 {
            self.hold_time = Some(seek);
        } else {
            self.start_time = Some(timeline_time.unwrap() - seek / self.playback_rate);
        }
        self.finish_notified = false;
    }

    /// `Animation.startTime = value`.
    pub fn set_start_time(&mut self, timeline_time: Option<f64>, value: Option<f64>) {
        let previous = self.current_time(timeline_time);
        self.start_time = value;
        if value.is_some() && self.playback_rate != 0.0 {
            self.hold_time = None;
        } else if value.is_none() {
            self.hold_time = previous;
        }
        self.finish_notified = false;
    }

    /// The spec's "play an animation" with the auto-rewind flag set — what `play()` and the
    /// implicit play inside `element.animate()` both run.
    pub fn play(&mut self, timeline_time: Option<f64>, effect_end: f64) -> Result<(), PlaybackError> {
        let ct = self.current_time(timeline_time);
        if self.playback_rate > 0.0 && ct.map(|c| c < 0.0 || c >= effect_end).unwrap_or(true) {
            self.hold_time = Some(0.0);
        } else if self.playback_rate < 0.0 && ct.map(|c| c <= 0.0 || c > effect_end).unwrap_or(true) {
            if !effect_end.is_finite() {
                return Err(PlaybackError::InvalidState);
            }
            self.hold_time = Some(effect_end);
        } else if self.playback_rate == 0.0 && ct.is_none() {
            self.hold_time = Some(0.0);
        }
        // With no pending play task, the animation starts NOW: convert the hold time into a start
        // time so the current time advances with the timeline.
        if let (Some(h), Some(t)) = (self.hold_time, timeline_time) {
            if self.playback_rate != 0.0 {
                self.start_time = Some(t - h / self.playback_rate);
                self.hold_time = None;
            } else {
                self.start_time = Some(t);
            }
        }
        self.finish_notified = false;
        Ok(())
    }

    /// `Animation.pause()`.
    pub fn pause(&mut self, timeline_time: Option<f64>, effect_end: f64) -> Result<(), PlaybackError> {
        if self.play_state(timeline_time, effect_end) == PlayState::Paused {
            return Ok(());
        }
        match self.current_time(timeline_time) {
            Some(c) => self.hold_time = Some(c),
            None => {
                if self.playback_rate >= 0.0 {
                    self.hold_time = Some(0.0);
                } else {
                    if !effect_end.is_finite() {
                        return Err(PlaybackError::InvalidState);
                    }
                    self.hold_time = Some(effect_end);
                }
            }
        }
        self.start_time = None;
        Ok(())
    }

    /// `Animation.finish()` — seek to the natural boundary in the direction of play.
    pub fn finish(&mut self, timeline_time: Option<f64>, effect_end: f64) -> Result<(), PlaybackError> {
        if self.playback_rate == 0.0 {
            return Err(PlaybackError::InvalidState);
        }
        if self.playback_rate > 0.0 && !effect_end.is_finite() {
            return Err(PlaybackError::InvalidState);
        }
        let limit = if self.playback_rate > 0.0 { effect_end } else { 0.0 };
        self.hold_time = Some(limit);
        // `playState` reads "finished" rather than "paused" only when a start time is resolved, so
        // finishing an idle animation must resolve one (the spec does the same, for the same reason).
        if let Some(t) = timeline_time {
            self.start_time = Some(t - limit / self.playback_rate);
        }
        self.finish_notified = false;
        Ok(())
    }

    /// `Animation.cancel()` — back to idle. The caller still owes the `cancel` event and the
    /// `finished`-promise rejection; this is only the state half.
    pub fn cancel(&mut self) {
        self.hold_time = None;
        self.start_time = None;
        self.finish_notified = false;
    }

    /// `Animation.reverse()` — invert the playback rate and play.
    ///
    /// The spec parks the new rate in a *pending* playback rate so that the auto-rewind decision is
    /// taken with the new rate while the current time is still read with the old one, then applies
    /// it at the end. With no pending window the same result comes from flipping the rate in a way
    /// that PRESERVES the current time (`set_playback_rate`) and then playing — the ordering matters
    /// either way: flipping the raw rate first would make a mid-flight `currentTime` of 400 read as
    /// -400 and send `play` rewinding to the end.
    ///
    /// On failure the whole state is restored, as the spec requires: a `reverse()` that throws must
    /// leave nothing observable behind.
    pub fn reverse(&mut self, timeline_time: Option<f64>, effect_end: f64) -> Result<(), PlaybackError> {
        let saved = self.clone();
        self.set_playback_rate(timeline_time, -self.playback_rate);
        match self.play(timeline_time, effect_end) {
            Ok(()) => Ok(()),
            Err(e) => {
                *self = saved;
                Err(e)
            }
        }
    }

    /// `Animation.playbackRate = value` — preserves the current time across the change.
    pub fn set_playback_rate(&mut self, timeline_time: Option<f64>, rate: f64) {
        let previous = self.current_time(timeline_time);
        self.playback_rate = rate;
        if let Some(c) = previous {
            self.set_current_time(timeline_time, c);
        }
    }

    /// `Animation.updatePlaybackRate(value)`. With no pending task queue this is
    /// `set_playback_rate` — the distinction the spec draws (apply at the next ready point rather
    /// than immediately) exists only for implementations that defer to a compositor.
    pub fn update_playback_rate(&mut self, timeline_time: Option<f64>, rate: f64) {
        self.set_playback_rate(timeline_time, rate);
    }

    /// True once the animation has reached its finished state and has not yet notified — the
    /// caller resolves the `finished` promise and fires `finish` when this flips.
    pub fn take_finish_notification(&mut self, timeline_time: Option<f64>, effect_end: f64) -> bool {
        let finished = self.play_state(timeline_time, effect_end) == PlayState::Finished;
        if finished && !self.finish_notified {
            self.finish_notified = true;
            return true;
        }
        if !finished {
            self.finish_notified = false;
        }
        false
    }
}

// ── interpolation ───────────────────────────────────────────────────────────────────────────────
//
// The value a property has partway between two keyframes. Everything below was measured on
// `/usr/bin/google-chrome` 150.0.7871.46 by seeking a PAUSED `element.animate()` to five points and
// reading `getComputedStyle` — no wall clock, and nothing from the spec:
//
//   opacity          0 -> 1                      0 · 0.25 · 0.5 · 0.75 · 1
//   width            10px -> 110px               10px · 35px · 60px · 85px · 110px
//   width            10% -> 50%                  interpolated as percentages
//   background-color rgb(255,0,0) -> rgb(0,0,255)  rgb(191,0,64) · rgb(128,0,128) · rgb(64,0,191)
//   background-color rgba(255,0,0,1) -> rgba(0,0,255,0)
//                                                rgba(255,0,0,0.75) · rgba(255,0,0,0.5) · …
//                                                — the red does NOT drift towards blue, which is
//                                                  what says the mix is PREMULTIPLIED by alpha
//   transform        translateX(0px) -> translateX(100px)   25px · 50px · 75px, per function
//   background-position 0px 0px -> 50px 100px    12.5px 25px · 25px 50px · …, per component
//   display          block -> inline             block · block · inline · inline · inline
//                                                — a non-interpolable pair is DISCRETE and flips at
//                                                  exactly 0.5, with 0.5 taking the second value
//   width            auto -> 110px               1904px · 1904px · 110px · … — also discrete
//
// What is deliberately NOT done: `transform: rotate(0deg) -> scale(2)`, where the function lists do
// not match. Chrome decomposes both to matrices and interpolates those; this engine has no matrix
// decomposition, so that pair is discrete here. It is a narrower answer, not a wrong one — the value
// is always one of the two the author wrote.

/// `"12.5px"` -> `(12.5, "px")`. `None` when the text is not a number with an optional unit.
fn split_number(s: &str) -> Option<(f64, &str)> {
    let end = s
        .char_indices()
        .take_while(|(i, c)| {
            c.is_ascii_digit() || *c == '.' || ((*c == '-' || *c == '+') && *i == 0) || *c == 'e' || *c == 'E'
                || ((*c == '-' || *c == '+') && matches!(s.as_bytes().get(i.wrapping_sub(1)), Some(b'e') | Some(b'E')))
        })
        .map(|(i, c)| i + c.len_utf8())
        .last()
        .unwrap_or(0);
    let (num, unit) = s.split_at(end);
    num.parse::<f64>().ok().map(|n| (n, unit))
}

/// `"translateX(100px)"` -> `("translateX", "100px")`.
///
/// STRICT: the whole string must be one call. `"translateX(0px) rotate(0deg)"` is a LIST of two,
/// and reading it as a single `translateX` whose argument is `"0px) rotate(0deg"` is how the list
/// case silently became discrete.
fn split_function(s: &str) -> Option<(&str, &str)> {
    let open = s.find('(')?;
    if !s.ends_with(')') || open == 0 {
        return None;
    }
    let mut depth = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 && i != s.len() - 1 {
                    return None; // the call closed before the end: this is a list
                }
            }
            _ => {}
        }
    }
    Some((s[..open].trim(), &s[open + 1..s.len() - 1]))
}

/// Split on top-level commas or whitespace, stepping OVER parenthesised groups.
fn split_top_level(s: &str, on_comma: bool) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 && on_comma => {
                out.push(s[start..i].trim());
                start = i + 1;
            }
            c if depth == 0 && !on_comma && c.is_ascii_whitespace() => {
                let piece = s[start..i].trim();
                if !piece.is_empty() {
                    out.push(piece);
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    let last = s[start..].trim();
    if !last.is_empty() || (on_comma && !s.trim().is_empty()) {
        out.push(last);
    }
    out.retain(|p| !p.is_empty());
    out
}

/// Mix two sRGB colours the way Chrome does: **premultiplied by alpha**.
fn mix_color(a: crate::value::Rgba, b: crate::value::Rgba, t: f64) -> crate::value::Rgba {
    let (aa, ba) = (a.a.clamp(0.0, 1.0) as f64, b.a.clamp(0.0, 1.0) as f64);
    let alpha = aa + (ba - aa) * t;
    let chan = |x: u8, y: u8| -> u8 {
        if alpha <= 0.0 {
            return 0;
        }
        let pre = (x as f64) * aa + ((y as f64) * ba - (x as f64) * aa) * t;
        (pre / alpha).round().clamp(0.0, 255.0) as u8
    };
    crate::value::Rgba { r: chan(a.r, b.r), g: chan(a.g, b.g), b: chan(a.b, b.b), a: alpha as f32, wide: None }
}

/// The interpolable part: `None` when the pair has no continuous path between it.
fn interpolate_continuous(from: &str, to: &str, t: f64) -> Option<String> {
    let (from, to) = (from.trim(), to.trim());
    if from.eq_ignore_ascii_case(to) {
        return Some(from.to_string());
    }
    // A plain number, with or without a unit. The units must match: `2em -> 40px` has no midpoint
    // until layout has resolved both, and answering one anyway would be a guess.
    if let (Some((a, ua)), Some((b, ub))) = (split_number(from), split_number(to)) {
        if ua.eq_ignore_ascii_case(ub) {
            return Some(format!("{}{}", crate::value::serialize_number((a + (b - a) * t) as f32), ua));
        }
        // A unitless 0 is a zero length or angle in whatever unit the other side has: `translateX(0)` ->
        // `translateX(600px)` moves (D77); it flipped at the half way as a discrete pair.
        if a == 0.0 && ua.is_empty() || b == 0.0 && ub.is_empty() {
            let unit = if ua.is_empty() { ub } else { ua };
            return Some(format!("{}{}", crate::value::serialize_number((a + (b - a) * t) as f32), unit));
        }
    }
    // `none` and a transform list: `none` is the list's identity -- each function at its neutral value -- so a
    // transition from no transform moves from where the element stands (CSS Transforms 1 §9).
    if from.eq_ignore_ascii_case("none") {
        if let Some(identity) = transform_identity(to) {
            return interpolate_continuous(&identity, to, t);
        }
    }
    if to.eq_ignore_ascii_case("none") {
        if let Some(identity) = transform_identity(from) {
            return interpolate_continuous(from, &identity, t);
        }
    }
    // A colour.
    if let (Ok(crate::value::Color::Rgba(a)), Ok(crate::value::Color::Rgba(b))) =
        (crate::value::Color::parse_str(from), crate::value::Color::parse_str(to))
    {
        return Some(crate::value::Color::Rgba(mix_color(a, b, t)).serialize());
    }
    // A function with the same name and the same arity on both sides: `translateX(0px)` ->
    // `translateX(100px)`, `scale(1)` -> `scale(3)`.
    if let (Some((fa, aa)), Some((fb, bb))) = (split_function(from), split_function(to)) {
        if fa.eq_ignore_ascii_case(fb) {
            let (la, lb) = (split_top_level(aa, true), split_top_level(bb, true));
            if la.len() == lb.len() && !la.is_empty() {
                let mut args = Vec::with_capacity(la.len());
                for (x, y) in la.iter().zip(&lb) {
                    args.push(interpolate_continuous(x, y, t)?);
                }
                return Some(format!("{}({})", fa, args.join(", ")));
            }
        }
    }
    // A space-separated list of the same length, every pair of which interpolates: `0px 0px` ->
    // `50px 100px`, and `translateX(0px) rotate(0deg)` -> `translateX(100px) rotate(90deg)`.
    let (la, lb) = (split_top_level(from, false), split_top_level(to, false));
    if la.len() == lb.len() && la.len() > 1 {
        let mut parts = Vec::with_capacity(la.len());
        for (x, y) in la.iter().zip(&lb) {
            parts.push(interpolate_continuous(x, y, t)?);
        }
        return Some(parts.join(" "));
    }
    None
}

/// A transform list's identity: each of its functions at the value that does nothing (`translateX(0px)`, `scale(1)`,
/// `rotate(0deg)`), the counterpart `none` interpolates against. `None` when `list` is not a list of transform functions.
fn transform_identity(list: &str) -> Option<String> {
    let mut out = Vec::new();
    for f in split_top_level(list.trim(), false) {
        let (name, args) = split_function(&f)?;
        let args = split_top_level(args, true);
        let lower = name.to_ascii_lowercase();
        let neutral = match lower.as_str() {
            "matrix" => "1, 0, 0, 1, 0, 0".to_string(),
            // keeps its axis, turns by nothing
            "rotate3d" if args.len() == 4 => format!("{}, 0deg", args[..3].join(", ")),
            n if n.starts_with("translate") => vec!["0px"; args.len().max(1)].join(", "),
            n if n.starts_with("scale") => vec!["1"; args.len().max(1)].join(", "),
            n if n.starts_with("rotate") || n.starts_with("skew") => vec!["0deg"; args.len().max(1)].join(", "),
            _ => return None,
        };
        out.push(format!("{name}({neutral})"));
    }
    (!out.is_empty()).then(|| out.join(" "))
}

/// The value a property has at progress `t` between two keyframe values.
///
/// Never `None`: a pair with no continuous path between it is **discrete**, which Chrome resolves
/// by flipping at exactly 0.5 with 0.5 taking the second value (measured: `display: block ->
/// inline` reads `block · block · inline · inline · inline` across 0, .25, .5, .75, 1).
pub fn interpolate(from: &str, to: &str, t: f64) -> String {
    interpolate_continuous(from, to, t)
        .unwrap_or_else(|| if t < 0.5 { from.trim().to_string() } else { to.trim().to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(t: f64) -> Option<f64> {
        Some(t)
    }

    #[test]
    fn a_fresh_animation_is_idle_with_no_times() {
        let p = Playback::default();
        assert_eq!(p.current_time(ms(0.0)), None);
        assert_eq!(p.play_state(ms(0.0), 0.0), PlayState::Idle);
        assert_eq!(p.playback_rate, 1.0);
        assert_eq!(p.replace_state, ReplaceState::Active);
    }

    #[test]
    fn current_time_advances_with_the_timeline() {
        let mut p = Playback::default();
        p.play(ms(100.0), 1000.0).unwrap();
        assert_eq!(p.current_time(ms(100.0)), Some(0.0));
        assert_eq!(p.current_time(ms(350.0)), Some(250.0));
        assert_eq!(p.play_state(ms(350.0), 1000.0), PlayState::Running);
    }

    #[test]
    fn pause_freezes_current_time() {
        let mut p = Playback::default();
        p.play(ms(0.0), 1000.0).unwrap();
        p.pause(ms(250.0), 1000.0).unwrap();
        assert_eq!(p.current_time(ms(250.0)), Some(250.0));
        // ...and it stays there however far the timeline runs on.
        assert_eq!(p.current_time(ms(9999.0)), Some(250.0));
        assert_eq!(p.play_state(ms(9999.0), 1000.0), PlayState::Paused);
    }

    #[test]
    fn play_after_pause_resumes_from_the_held_time() {
        let mut p = Playback::default();
        p.play(ms(0.0), 1000.0).unwrap();
        p.pause(ms(250.0), 1000.0).unwrap();
        p.play(ms(1000.0), 1000.0).unwrap();
        assert_eq!(p.current_time(ms(1000.0)), Some(250.0));
        assert_eq!(p.current_time(ms(1100.0)), Some(350.0));
    }

    #[test]
    fn finish_pins_to_the_effect_end_and_reports_finished() {
        let mut p = Playback::default();
        p.play(ms(0.0), 1000.0).unwrap();
        p.finish(ms(10.0), 1000.0).unwrap();
        assert_eq!(p.current_time(ms(10.0)), Some(1000.0));
        assert_eq!(p.play_state(ms(10.0), 1000.0), PlayState::Finished);
        assert!(p.take_finish_notification(ms(10.0), 1000.0), "the first observation notifies");
        assert!(!p.take_finish_notification(ms(10.0), 1000.0), "the second does not");
    }

    #[test]
    fn finishing_an_idle_animation_with_no_effect_still_reads_finished() {
        // `new Animation().finish()` — effect end 0, so the limit is 0 and the animation is
        // instantly at its end. Brave 151 records `finish()` returning undefined here, i.e. no throw.
        let mut p = Playback::default();
        p.finish(ms(500.0), 0.0).unwrap();
        assert_eq!(p.play_state(ms(500.0), 0.0), PlayState::Finished);
    }

    #[test]
    fn finish_rejects_a_zero_playback_rate_and_an_infinite_effect() {
        let mut p = Playback { playback_rate: 0.0, ..Default::default() };
        assert_eq!(p.finish(ms(0.0), 100.0), Err(PlaybackError::InvalidState));
        let mut q = Playback::default();
        assert_eq!(q.finish(ms(0.0), f64::INFINITY), Err(PlaybackError::InvalidState));
    }

    #[test]
    fn cancel_returns_to_idle() {
        let mut p = Playback::default();
        p.play(ms(0.0), 1000.0).unwrap();
        p.cancel();
        assert_eq!(p.play_state(ms(500.0), 1000.0), PlayState::Idle);
        assert_eq!(p.current_time(ms(500.0)), None);
    }

    #[test]
    fn reverse_flips_the_rate_and_rewinds_from_the_end() {
        let mut p = Playback::default();
        p.play(ms(0.0), 1000.0).unwrap();
        p.reverse(ms(400.0), 1000.0).unwrap();
        assert_eq!(p.playback_rate, -1.0);
        assert_eq!(p.current_time(ms(400.0)), Some(400.0));
        assert_eq!(p.current_time(ms(500.0)), Some(300.0));
    }

    #[test]
    fn reverse_into_an_infinite_effect_restores_the_rate() {
        let mut p = Playback::default();
        assert_eq!(p.reverse(ms(0.0), f64::INFINITY), Err(PlaybackError::InvalidState));
        assert_eq!(p.playback_rate, 1.0, "a failed reverse must not leave the rate flipped");
    }

    #[test]
    fn playback_rate_change_preserves_the_current_time() {
        let mut p = Playback::default();
        p.play(ms(0.0), 1000.0).unwrap();
        p.set_playback_rate(ms(400.0), 2.0);
        assert_eq!(p.current_time(ms(400.0)), Some(400.0));
        assert_eq!(p.current_time(ms(500.0)), Some(600.0));
    }

    #[test]
    fn timing_derives_active_duration_and_end_time() {
        let t = Timing { duration: Some(1000.0), iterations: 3.0, delay: 50.0, end_delay: 10.0, ..Default::default() };
        assert_eq!(t.active_duration(), 3000.0);
        assert_eq!(t.end_time(), 3060.0);
        let zero = Timing { duration: Some(0.0), iterations: f64::INFINITY, ..Default::default() };
        assert_eq!(zero.active_duration(), 0.0, "0 x infinity is 0 in the timing model");
    }

    #[test]
    fn computed_timing_runs_the_phases() {
        let t = Timing { duration: Some(1000.0), delay: 100.0, ..Default::default() };
        let before = compute_timing(&t, Some(0.0));
        assert_eq!(before.phase, Phase::Before);
        assert_eq!(before.progress, None, "fill defaults to none, so nothing before the delay");
        let mid = compute_timing(&t, Some(600.0));
        assert_eq!(mid.phase, Phase::Active);
        assert_eq!(mid.progress, Some(0.5));
        assert_eq!(mid.current_iteration, Some(0.0));
        let after = compute_timing(&t, Some(2000.0));
        assert_eq!(after.phase, Phase::After);
        assert_eq!(after.progress, None);
        let idle = compute_timing(&t, None);
        assert_eq!(idle.phase, Phase::Idle);
        assert_eq!(idle.local_time, None);
    }

    #[test]
    fn fill_forwards_holds_the_final_progress() {
        let t = Timing { duration: Some(1000.0), fill: FillMode::Forwards, ..Default::default() };
        let after = compute_timing(&t, Some(5000.0));
        assert_eq!(after.progress, Some(1.0));
        assert_eq!(after.current_iteration, Some(0.0));
    }

    #[test]
    fn alternate_direction_reverses_odd_iterations() {
        let t = Timing {
            duration: Some(100.0),
            iterations: 4.0,
            direction: PlaybackDirection::Alternate,
            ..Default::default()
        };
        assert_eq!(compute_timing(&t, Some(25.0)).progress, Some(0.25));
        // Second iteration (index 1) runs backwards.
        assert_eq!(compute_timing(&t, Some(125.0)).progress, Some(0.75));
        assert_eq!(compute_timing(&t, Some(125.0)).current_iteration, Some(1.0));
    }

    #[test]
    fn easing_parses_the_css_grammar() {
        assert_eq!(Easing::parse("linear"), Some(Easing::Linear));
        assert_eq!(Easing::parse("ease-in-out"), Some(Easing::CubicBezier(0.42, 0.0, 0.58, 1.0)));
        assert_eq!(Easing::parse("cubic-bezier(0, 0, 1, 1)"), Some(Easing::CubicBezier(0.0, 0.0, 1.0, 1.0)));
        assert_eq!(Easing::parse("steps(4, jump-none)"), Some(Easing::Steps(4, StepPosition::JumpNone)));
        assert_eq!(Easing::parse("step-start"), Some(Easing::Steps(1, StepPosition::Start)));
        assert_eq!(Easing::parse("cubic-bezier(2, 0, 1, 1)"), None, "x control points are clamped to [0,1]");
        assert_eq!(Easing::parse("steps(0)"), None);
        assert_eq!(Easing::parse("steps(1, jump-none)"), None);
        assert_eq!(Easing::parse("wobble"), None);
    }

    #[test]
    fn linear_cubic_bezier_is_the_identity() {
        let e = Easing::CubicBezier(0.0, 0.0, 1.0, 1.0);
        for t in [0.0, 0.1, 0.25, 0.5, 0.75, 0.9, 1.0] {
            assert!((e.eval(t) - t).abs() < 1e-4, "identity curve at {t} gave {}", e.eval(t));
        }
    }

    #[test]
    fn ease_in_out_is_symmetric_about_the_midpoint() {
        let e = Easing::parse("ease-in-out").unwrap();
        assert!((e.eval(0.5) - 0.5).abs() < 1e-4);
        assert!(e.eval(0.25) < 0.25, "ease-in-out starts slow");
        assert!(e.eval(0.75) > 0.75, "...and ends fast");
    }

    #[test]
    fn steps_quantizes_progress() {
        let e = Easing::parse("steps(4)").unwrap();
        assert_eq!(e.eval(0.0), 0.0);
        assert_eq!(e.eval(0.3), 0.25);
        assert_eq!(e.eval(0.6), 0.5);
        assert_eq!(e.eval(1.0), 1.0);
    }

    #[test]
    fn overall_progress_spans_the_whole_effect() {
        let t = Timing { duration: Some(1000.0), iterations: 2.0, ..Default::default() };
        let mut p = Playback::default();
        p.play(ms(0.0), t.end_time()).unwrap();
        assert_eq!(p.overall_progress(ms(0.0), &t), Some(0.0));
        assert_eq!(p.overall_progress(ms(1000.0), &t), Some(0.5));
        assert_eq!(p.overall_progress(ms(3000.0), &t), Some(1.0), "clamped at the end");
    }

    // ── interpolation ───────────────────────────────────────────────────────────────────────────
    //
    // Every expected string below was read out of Chrome 150 by seeking a PAUSED
    // `element.animate()` to that progress and reading `getComputedStyle`.

    fn five(from: &str, to: &str) -> Vec<String> {
        [0.0, 0.25, 0.5, 0.75, 1.0].iter().map(|t| interpolate(from, to, *t)).collect()
    }

    #[test]
    fn a_number_interpolates_and_keeps_its_unit() {
        assert_eq!(five("0", "1"), ["0", "0.25", "0.5", "0.75", "1"]);
        assert_eq!(five("10px", "110px"), ["10px", "35px", "60px", "85px", "110px"]);
        assert_eq!(five("10%", "50%"), ["10%", "20%", "30%", "40%", "50%"]);
        assert_eq!(five("0px", "40px"), ["0px", "10px", "20px", "30px", "40px"]);
        assert_eq!(five("100", "900"), ["100", "300", "500", "700", "900"]);
    }

    #[test]
    fn mismatched_units_have_no_midpoint_so_the_pair_is_discrete() {
        // `2em -> 40px` cannot be mixed before layout has resolved the em, and guessing would be
        // worse than answering one of the two values the author wrote.
        assert_eq!(five("2em", "40px"), ["2em", "2em", "40px", "40px", "40px"]);
    }

    #[test]
    fn colours_mix_premultiplied_by_alpha() {
        assert_eq!(
            five("rgb(255, 0, 0)", "rgb(0, 0, 255)"),
            [
                "rgb(255, 0, 0)",
                "rgb(191, 0, 64)",
                "rgb(128, 0, 128)",
                "rgb(64, 0, 191)",
                "rgb(0, 0, 255)"
            ]
        );
        assert_eq!(five("red", "blue")[2], "rgb(128, 0, 128)");
        assert_eq!(five("#000000", "#ffffff")[2], "rgb(128, 128, 128)");
        // THE ONE THAT SAYS IT IS PREMULTIPLIED: fading red out towards a transparent blue leaves
        // the red pure the whole way, because the blue contributes nothing at alpha 0.
        assert_eq!(
            five("rgba(255, 0, 0, 1)", "rgba(0, 0, 255, 0)"),
            [
                "rgb(255, 0, 0)",
                "rgba(255, 0, 0, 0.75)",
                "rgba(255, 0, 0, 0.5)",
                "rgba(255, 0, 0, 0.25)",
                "rgba(0, 0, 0, 0)"
            ]
        );
    }

    #[test]
    fn a_transform_function_interpolates_argument_by_argument() {
        assert_eq!(
            five("translateX(0px)", "translateX(100px)"),
            [
                "translateX(0px)",
                "translateX(25px)",
                "translateX(50px)",
                "translateX(75px)",
                "translateX(100px)"
            ]
        );
        assert_eq!(five("scale(1)", "scale(3)")[2], "scale(2)");
        // A list of functions, pairwise.
        assert_eq!(
            interpolate("translateX(0px) rotate(0deg)", "translateX(100px) rotate(90deg)", 0.5),
            "translateX(50px) rotate(45deg)"
        );
        assert_eq!(interpolate("translate(0px, 0px)", "translate(10px, 20px)", 0.5), "translate(5px, 10px)");
    }

    #[test]
    fn a_pair_of_functions_that_do_not_match_is_discrete() {
        // Chrome decomposes both to matrices here; this engine has none, so the answer is one of
        // the two authored values rather than a guess at the matrix between them.
        assert_eq!(five("rotate(0deg)", "scale(2)"), ["rotate(0deg)", "rotate(0deg)", "scale(2)", "scale(2)", "scale(2)"]);
    }

    #[test]
    fn a_space_separated_pair_interpolates_component_by_component() {
        assert_eq!(interpolate("0px 0px", "50px 100px", 0.25), "12.5px 25px");
        assert_eq!(interpolate("0px 0px", "50px 100px", 0.5), "25px 50px");
    }

    #[test]
    fn a_non_interpolable_pair_flips_at_exactly_half() {
        assert_eq!(five("block", "inline"), ["block", "block", "inline", "inline", "inline"]);
        assert_eq!(five("auto", "110px"), ["auto", "auto", "110px", "110px", "110px"]);
        assert_eq!(five("none", "hidden"), ["none", "none", "hidden", "hidden", "hidden"]);
    }

    #[test]
    fn two_equal_values_interpolate_to_themselves() {
        assert_eq!(five("10px", "10px"), ["10px"; 5]);
        assert_eq!(five("auto", "auto"), ["auto"; 5]);
    }

}
