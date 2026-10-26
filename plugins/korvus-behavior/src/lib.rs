//! `behavior` — human-like input generation. Pure Rust (no v8): produces `MotionSample`s
//! that `engine` dispatches as genuinely-trusted native events. The primary trajectory source is an
//! analytic **neuromotor** model — a WindMouse spatial path (gravity + smoothly-random wind, integer
//! output, near-target damping → overshoot + correction) resampled to the pointer poll rate, timed by
//! Fitts's law, with an asymmetric velocity bell, 8–12 Hz tremor, and jittered Δt on Chrome's 100 µs
//! event-timestamp grid. Self-contained (no recorded corpus, no ML runtime); a swappable
//! `TrajectorySource` so recorded-replay / diffusion can drop in later.
//!
//! See docs/BEHAVIORAL-RESEARCH.md. The `oracle` module is the
//! offline behavioral regression net (a device-check script `es_*`, Castle rules, human distributions, anti-replay).

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use spec::{MotionSample, MotorProfile, Point, TrajectorySource};

/// The swappable input **model** as a kernel-`Context` service: the four motor sources behind one
/// interior-mutability lock (deno_core drives CDP `Input` single-threaded, so the lock is uncontended).
/// The engine holds it as `Arc<dyn BehaviorService>`, so a plugin can swap neuromotor → recorded-replay
/// → learned without touching the engine's input code. `&self` methods (interior mutation) keep the
/// trait object storable on the `Context`.
pub trait BehaviorService: Send + Sync {
    fn plan_move(&self, from: Point, to: Point, target_w: f64) -> Vec<MotionSample>;
    fn press_dwell_us(&self) -> f64;
    fn idle_offset(&self, radius: f64) -> (f64, f64);
    fn type_text(&self, text: &str) -> Vec<KeyEvent>;
    fn scroll(&self, total_dy: f64) -> Vec<WheelEvent>;
    fn tap(&self, x: f64, y: f64) -> Vec<TouchSample>;
    fn swipe(&self, from: Point, to: Point) -> Vec<TouchSample>;
    fn motor_seed(&self) -> u64;
}

/// The behavior service handle placed on the `Context` (mirrors `RenderHandle`/`PersonaHandle`).
pub struct BehaviorHandle(pub std::sync::Arc<dyn BehaviorService>);

struct MotorSources {
    neuromotor: SigmaLognormalSource,
    keystroke: KeystrokeSource,
    scroller: ScrollSource,
    touch: TouchSource,
}

/// The default (neuromotor) behavior model: the four seeded sources behind one `Mutex`.
pub struct NeuromotorBehavior {
    seed: u64,
    sources: std::sync::Mutex<MotorSources>,
}
impl NeuromotorBehavior {
    pub fn new(seed: u64, wheel: WheelKind) -> Self {
        let p = || MotorProfile { seed, ..Default::default() };
        Self {
            seed,
            sources: std::sync::Mutex::new(MotorSources {
                neuromotor: SigmaLognormalSource::new(p()),
                keystroke: KeystrokeSource::new(p()),
                scroller: ScrollSource::new(p(), wheel),
                touch: TouchSource::new(p()),
            }),
        }
    }
}
impl BehaviorService for NeuromotorBehavior {
    fn plan_move(&self, from: Point, to: Point, target_w: f64) -> Vec<MotionSample> {
        self.sources.lock().unwrap().neuromotor.plan_move(from, to, target_w)
    }
    fn press_dwell_us(&self) -> f64 {
        self.sources.lock().unwrap().neuromotor.press_dwell_us()
    }
    fn idle_offset(&self, radius: f64) -> (f64, f64) {
        self.sources.lock().unwrap().neuromotor.idle_offset(radius)
    }
    fn type_text(&self, text: &str) -> Vec<KeyEvent> {
        self.sources.lock().unwrap().keystroke.type_text(text)
    }
    fn scroll(&self, total_dy: f64) -> Vec<WheelEvent> {
        self.sources.lock().unwrap().scroller.scroll(total_dy)
    }
    fn tap(&self, x: f64, y: f64) -> Vec<TouchSample> {
        self.sources.lock().unwrap().touch.tap(x, y)
    }
    fn swipe(&self, from: Point, to: Point) -> Vec<TouchSample> {
        self.sources.lock().unwrap().touch.swipe(from, to)
    }
    fn motor_seed(&self) -> u64 {
        self.seed
    }
}

/// The default behavior plugin: `provide`s a `BehaviorHandle(NeuromotorBehavior)` on the `Context`.
pub struct BehaviorPlugin {
    seed: u64,
    wheel: WheelKind,
}
impl BehaviorPlugin {
    pub fn new(seed: u64, wheel: WheelKind) -> Self {
        Self { seed, wheel }
    }
}
impl spec::kernel::Plugin for BehaviorPlugin {
    fn name(&self) -> &str {
        "behavior"
    }
    fn provides(&self) -> &[&str] {
        &["behavior"]
    }
    fn apply(&self, ctx: &spec::kernel::Context, _c: &spec::kernel::PluginConfig) -> spec::kernel::Result<()> {
        ctx.provide(BehaviorHandle(std::sync::Arc::new(NeuromotorBehavior::new(self.seed, self.wheel))));
        Ok(())
    }
}

mod human_profiles;

/// Placeholder retained for the compile-time plugin seam probe.
pub fn behavior_name() -> &'static str {
    "korvus-behavior"
}

/// Fitts's law movement time (ms): `MT = a + b·log2(D/W + 1)`. Throughput ~4–5 bit/s for a mouse → `b`
/// in ms/bit; `a` is the reaction/setup intercept. Clamped to a human floor. Detectors check that MT
/// scales with `log2(D/W+1)`; a generator ignoring this is a tell.
pub fn fitts_mt_ms(distance: f64, target_w: f64) -> f64 {
    const A: f64 = 80.0; // intercept ms (reaction / setup)
    const B: f64 = 220.0; // ms/bit (TP ≈ 4.5 bit/s)
    let w = target_w.max(1.0);
    let id = (distance / w + 1.0).log2();
    (A + B * id).max(40.0)
}

/// Quantize a microsecond timestamp to Chrome's 100 µs event-`timeStamp` grid (Spectre clamp). Jittered
/// Δt upstream keeps the post-grid distribution non-degenerate (healthy `es_sigmdn`).
fn grid_100us(t_us: f64) -> f64 {
    (t_us / 100.0).round() * 100.0
}

/// The neuromotor trajectory source (L3, primary). Holds the motor persona + a seeded PRNG advanced
/// across moves so successive trajectories never repeat (anti-replay by construction).
pub struct NeuromotorSource {
    motor: MotorProfile,
    rng: StdRng,
}

impl NeuromotorSource {
    pub fn new(motor: MotorProfile) -> Self {
        Self { rng: StdRng::seed_from_u64(motor.seed), motor }
    }
    pub fn motor(&self) -> &MotorProfile {
        &self.motor
    }

    /// Pre-click mousedown-hold (press duration) in µs: ~N(90 ms, 25) floored 50 — the human settle
    /// between arriving on a target and releasing the button.
    pub fn press_dwell_us(&mut self) -> f64 {
        normal(&mut self.rng, 90.0, 25.0).max(50.0) * 1000.0
    }

    /// A small idle-drift offset (px) within `radius` for ambient hover / reading micro-motion — humans
    /// are never perfectly still (absence of any drift is itself a tell).
    pub fn idle_offset(&mut self, radius: f64) -> (f64, f64) {
        let ang = self.rng.gen_range(0.0..std::f64::consts::TAU);
        let r = self.rng.gen_range(2.0..radius.max(3.0));
        (r * ang.cos(), r * ang.sin())
    }
}

/// A stable synthetic viewport coordinate for a DOM-target key (FNV-1a of key XOR seed). Korvus has no
/// layout engine, so element geometry is 0; this gives each target a consistent point the cursor can
/// "arrive" at. Un-cross-checkable: the detector's own `getBoundingClientRect` also reads 0 inside Korvus.
pub fn synth_coord(key: &str, seed: u64, width: i32, height: i32) -> (i32, i32) {
    let mut h: u64 = 0xcbf29ce484222325 ^ seed;
    for b in key.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    let w = (width.max(64) - 32) as u64;
    let ht = (height.max(64) - 32) as u64;
    (16 + (h % w) as i32, 16 + ((h >> 32) % ht) as i32)
}

impl TrajectorySource for NeuromotorSource {
    fn plan_move(&mut self, from: Point, to: Point, target_w: f64) -> Vec<MotionSample> {
        // 1. WindMouse spatial path (float, pre-tremor). Variable step SIZES already encode a velocity
        //    profile (small near start/end, large mid-move) and produce natural overshoot + correction.
        let path = windmouse(from, to, &mut self.rng, self.motor.overshoot);
        let dist = ((to.x - from.x).powi(2) + (to.y - from.y).powi(2)).sqrt();
        if path.len() < 2 || dist < 1.0 {
            return vec![MotionSample { x: to.x.round() as i32, y: to.y.round() as i32, t_us: 0.0 }];
        }
        // 2. Resample to the pointer poll rate over the Fitts duration → a human per-move sample count
        //    (~poll_hz × MT). Resampling by uniform INDEX (not arc length) preserves WindMouse's
        //    velocity profile: dense-in-slow-regions stays slow under uniform Δt → an asymmetric bell.
        let mt_ms = fitts_mt_ms(dist, target_w);
        let n = ((mt_ms * self.motor.poll_hz / 1000.0).round() as usize).clamp(2, 4096);
        let resampled = resample_by_index(&path, n);
        let base_dt_us = (mt_ms * 1000.0) / n as f64;
        // 3. Emit: 8–12 Hz tremor (sub-pixel → ±1px dither), integer coords, jittered Δt on the 100 µs
        //    grid (constant Δt is an LSTM-catchable tell).
        let mut t_us = 0.0;
        let mut out = Vec::with_capacity(n);
        for (i, &(px, py)) in resampled.iter().enumerate() {
            let phase = self.motor.tremor_hz * std::f64::consts::TAU * (t_us / 1_000_000.0);
            let tremor = self.motor.tremor_amp * phase.sin() * self.rng.gen_range(0.3..1.0);
            let x = (px + tremor).round() as i32;
            let y = (py - tremor).round() as i32;
            out.push(MotionSample { x, y, t_us: grid_100us(t_us) });
            if i + 1 < n {
                let jitter = self.rng.gen_range(0.75..1.25); // ±25%
                t_us += base_dt_us * jitter;
            }
        }
        // Humans arrive on target (mid-path overshoot already modeled); land the final sample exactly.
        if let Some(last) = out.last_mut() {
            last.x = to.x.round() as i32;
            last.y = to.y.round() as i32;
        }
        out
    }
}

/// WindMouse (BenLand100): the cursor as a mass pulled by constant gravity toward the target plus a
/// smoothly-random wind, Euler-integrated with a velocity clip, and near-target damping that yields human
/// curvature + overshoot/correction. Returns float positions (pre-tremor / pre-round). Params per the
/// research (G0=9, W0=3, D0=12; max step scaled by the overshoot trait).
fn windmouse(from: Point, to: Point, rng: &mut StdRng, overshoot: f64) -> Vec<(f64, f64)> {
    let g0: f64 = 9.0;
    let w0: f64 = 3.0;
    let mut m0: f64 = 15.0 * (0.7 + overshoot * 0.6); // max step / velocity clip
    let d0: f64 = 12.0;
    let (s3, s5) = (3.0_f64.sqrt(), 5.0_f64.sqrt());
    let (mut sx, mut sy) = (from.x, from.y);
    let (dx, dy) = (to.x, to.y);
    let (mut vx, mut vy) = (0.0_f64, 0.0_f64);
    let (mut wx, mut wy) = (0.0_f64, 0.0_f64);
    let mut out: Vec<(f64, f64)> = Vec::new();
    let mut guard = 0;
    loop {
        let dist = ((dx - sx).powi(2) + (dy - sy).powi(2)).sqrt();
        if dist < 1.0 {
            break;
        }
        guard += 1;
        if guard > 100_000 {
            break;
        }
        let w_mag = w0.min(dist);
        if dist >= d0 {
            wx = wx / s3 + (2.0 * rng.gen::<f64>() - 1.0) * w_mag / s5;
            wy = wy / s3 + (2.0 * rng.gen::<f64>() - 1.0) * w_mag / s5;
        } else {
            wx /= s3;
            wy /= s3;
            if m0 < 3.0 {
                m0 = rng.gen::<f64>() * 3.0 + 3.0;
            } else {
                m0 /= s5;
            }
        }
        vx += wx + g0 * (dx - sx) / dist;
        vy += wy + g0 * (dy - sy) / dist;
        let v_mag = (vx * vx + vy * vy).sqrt();
        if v_mag > m0 {
            let v_clip = m0 / 2.0 + rng.gen::<f64>() * m0 / 2.0;
            vx = vx / v_mag * v_clip;
            vy = vy / v_mag * v_clip;
        }
        sx += vx;
        sy += vy;
        out.push((sx, sy));
    }
    out.push((dx, dy));
    out
}

/// Resample a polyline to `n` points at uniform parameter over the point INDEX (not arc length), so the
/// source's velocity profile (dense samples in slow regions) is preserved.
fn resample_by_index(path: &[(f64, f64)], n: usize) -> Vec<(f64, f64)> {
    let m = path.len();
    if m < 2 || n < 2 {
        return path.to_vec();
    }
    (0..n)
        .map(|k| {
            let f = k as f64 * (m - 1) as f64 / (n - 1) as f64;
            let i = f.floor() as usize;
            let frac = f - i as f64;
            if i + 1 < m {
                let (x0, y0) = path[i];
                let (x1, y1) = path[i + 1];
                (x0 + (x1 - x0) * frac, y0 + (y1 - y0) * frac)
            } else {
                path[m - 1]
            }
        })
        .collect()
}

// ================= Sigma-lognormal trajectory source (Plamondon Kinematic Theory) =================

/// erf via Abramowitz-Stegun 7.1.26 (max error ~1.5e-7) — for the normal CDF below.
fn erf(x: f64) -> f64 {
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.3275911 * x);
    let y = 1.0
        - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t + 0.254829592)
            * t
            * (-x * x).exp();
    sign * y
}

/// Standard-normal CDF Φ(z).
fn norm_cdf(z: f64) -> f64 {
    0.5 * (1.0 + erf(z / std::f64::consts::SQRT_2))
}

/// The Sigma-lognormal trajectory source — the SOTA analytic human model (Plamondon's Kinematic Theory).
/// A move is a superposition of "strokes", each a LOGNORMAL velocity profile (the agonist/antagonist
/// neuromuscular impulse response) → the asymmetric velocity bell (fast rise, slow decay) real humans
/// produce. It is also the exact model detectors like BeCAPTCHA-Mouse reconstruct against (93% on naive
/// bots), so GENERATING with it minimises reconstruction error. Primary + corrective strokes give the
/// overshoot-and-correct (Meyer submovements); a slight arc gives curvature; 8-12 Hz tremor + jittered Δt
/// on the 100 µs grid keep the micro-scale + timing human. Self-contained; behind the `TrajectorySource`
/// trait so a learned (diffusion) source can drop in later.
pub struct SigmaLognormalSource {
    motor: MotorProfile,
    rng: StdRng,
}

impl SigmaLognormalSource {
    pub fn new(motor: MotorProfile) -> Self {
        Self { rng: StdRng::seed_from_u64(motor.seed), motor }
    }
    pub fn motor(&self) -> &MotorProfile {
        &self.motor
    }
    /// Pre-click mousedown-hold (press duration) in µs: ~N(90 ms, 25) floored 50.
    pub fn press_dwell_us(&mut self) -> f64 {
        normal(&mut self.rng, 90.0, 25.0).max(50.0) * 1000.0
    }
    /// A small idle-drift offset (px) within `radius` for ambient hover / reading micro-motion.
    pub fn idle_offset(&mut self, radius: f64) -> (f64, f64) {
        let ang = self.rng.gen_range(0.0..std::f64::consts::TAU);
        let r = self.rng.gen_range(2.0..radius.max(3.0));
        (r * ang.cos(), r * ang.sin())
    }
}

impl TrajectorySource for SigmaLognormalSource {
    fn plan_move(&mut self, from: Point, to: Point, target_w: f64) -> Vec<MotionSample> {
        let (dx, dy) = (to.x - from.x, to.y - from.y);
        let dist = (dx * dx + dy * dy).sqrt();
        if dist < 1.0 {
            return vec![MotionSample { x: to.x.round() as i32, y: to.y.round() as i32, t_us: 0.0 }];
        }
        let (dir_x, dir_y) = (dx / dist, dy / dist);
        let (perp_x, perp_y) = (-dir_y, dir_x); // unit perpendicular, for the curvature bow

        let mt_ms = fitts_mt_ms(dist, target_w);
        let n = ((mt_ms * self.motor.poll_hz / 1000.0).round() as usize).clamp(2, 4096);

        // Two strokes: a primary that slightly overshoots + a later corrective that pulls back to target
        // (D1 + D2 = dist, so the superposition lands exactly on target). Each is a lognormal in time.
        let of = self.motor.overshoot * self.rng.gen_range(0.02..0.12); // overshoot fraction
        let t1 = mt_ms; // primary movement time (ms)
        let sig1 = self.rng.gen_range(0.30..0.40); // log-response time (asymmetry)
        let mu1 = t1.ln() - 2.33 * sig1; // ~99th pct of the lognormal-in-t lands at t1
        let d1 = dist * (1.0 + of);
        let t0_2 = 0.55 * t1; // corrective onset
        let t2 = 0.55 * t1;
        let sig2 = self.rng.gen_range(0.35..0.45);
        let mu2 = t2.ln() - 2.33 * sig2;
        let d2 = -dist * of;
        let bow = self.rng.gen_range(-0.06..0.06) * dist; // perpendicular curvature amplitude (px)

        let total_ms = t1 * 1.15; // a short tail so the correction resolves
        let base_dt_us = total_ms * 1000.0 / n as f64;
        let mut t_us: f64 = 0.0;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let t_ms = t_us / 1000.0;
            // Superposed lognormal POSITION (integral of the lognormal speed) along the movement axis.
            let s1 = if t_ms > 0.0 { d1 * norm_cdf((t_ms.ln() - mu1) / sig1) } else { 0.0 };
            let s2 = if t_ms > t0_2 { d2 * norm_cdf(((t_ms - t0_2).ln() - mu2) / sig2) } else { 0.0 };
            let along = s1 + s2;
            // Curvature bow (peaks mid-path) + 8-12 Hz physiological tremor.
            let af = (along / dist).clamp(0.0, 1.0);
            let bow_off = bow * (af * std::f64::consts::PI).sin();
            let phase = self.motor.tremor_hz * std::f64::consts::TAU * (t_us / 1_000_000.0);
            let tremor = self.motor.tremor_amp * phase.sin() * self.rng.gen_range(0.3..1.0);
            let px = from.x + dir_x * along + perp_x * (bow_off + tremor);
            let py = from.y + dir_y * along + perp_y * (bow_off - tremor);
            out.push(MotionSample { x: px.round() as i32, y: py.round() as i32, t_us: grid_100us(t_us) });
            if i + 1 < n {
                t_us += base_dt_us * self.rng.gen_range(0.8..1.2); // jittered Δt (never constant)
            }
        }
        if let Some(last) = out.last_mut() {
            last.x = to.x.round() as i32;
            last.y = to.y.round() as i32;
        }
        out
    }
}

// ================= Recorded-replay trajectory source (learned from real human data) =================

/// A trajectory source that REPLAYS real human velocity dynamics from the SapiMouse dataset, warped to
/// each move (replay-with-perturbation). It picks one of the embedded 256-point human speed profiles,
/// integrates it to a position-vs-time curve, scales that to the target distance/direction, adds a slight
/// arc + 8-12 Hz tremor, and resamples to the poll rate with jittered Δt. Fresh profile + noise per move,
/// so no two trajectories repeat (anti-replay). This is the "learned from human data" option the research
/// recommends where training a diffusion model isn't feasible: it preserves genuine human micro-structure
/// (a real person's accel/decel + sub-movements) that an analytic model only approximates. Behind the same
/// `TrajectorySource` trait; select it in place of `SigmaLognormalSource`.
pub struct RecordedReplaySource {
    motor: MotorProfile,
    rng: StdRng,
}

impl RecordedReplaySource {
    pub fn new(motor: MotorProfile) -> Self {
        Self { rng: StdRng::seed_from_u64(motor.seed ^ 0x7265_706c_6179), motor }
    }
    pub fn press_dwell_us(&mut self) -> f64 {
        normal(&mut self.rng, 90.0, 25.0).max(50.0) * 1000.0
    }
    pub fn idle_offset(&mut self, radius: f64) -> (f64, f64) {
        let ang = self.rng.gen_range(0.0..std::f64::consts::TAU);
        let r = self.rng.gen_range(2.0..radius.max(3.0));
        (r * ang.cos(), r * ang.sin())
    }
}

impl TrajectorySource for RecordedReplaySource {
    fn plan_move(&mut self, from: Point, to: Point, target_w: f64) -> Vec<MotionSample> {
        let (dx, dy) = (to.x - from.x, to.y - from.y);
        let dist = (dx * dx + dy * dy).sqrt();
        if dist < 1.0 {
            return vec![MotionSample { x: to.x.round() as i32, y: to.y.round() as i32, t_us: 0.0 }];
        }
        let (dir_x, dir_y) = (dx / dist, dy / dist);
        let (perp_x, perp_y) = (-dir_y, dir_x);
        // Pick a real human speed profile; integrate to a cumulative-distance (position-vs-time) curve.
        let prof = &human_profiles::PROFILES[self.rng.gen_range(0..human_profiles::PROFILES.len())];
        let mut cum = [0.0f64; 256];
        let mut acc = 0.0;
        for i in 0..256 {
            acc += prof[i] as f64;
            cum[i] = acc;
        }
        let total = acc.max(1.0);
        let mt_ms = fitts_mt_ms(dist, target_w);
        let n = ((mt_ms * self.motor.poll_hz / 1000.0).round() as usize).clamp(2, 4096);
        let bow = self.rng.gen_range(-0.05..0.05) * dist;
        let base_dt_us = mt_ms * 1000.0 / n as f64;
        let mut t_us: f64 = 0.0;
        let mut out = Vec::with_capacity(n);
        for k in 0..n {
            let u = k as f64 / (n - 1) as f64; // normalized time
            let fi = (u * 255.0).min(255.0);
            let i0 = fi.floor() as usize;
            let frac = fi - i0 as f64;
            let c = if i0 + 1 < 256 { cum[i0] + (cum[i0 + 1] - cum[i0]) * frac } else { cum[255] };
            let along = c / total * dist; // real human position-vs-time, scaled to the target
            let af = (along / dist).clamp(0.0, 1.0);
            let bow_off = bow * (af * std::f64::consts::PI).sin();
            let phase = self.motor.tremor_hz * std::f64::consts::TAU * (t_us / 1_000_000.0);
            let tremor = self.motor.tremor_amp * phase.sin() * self.rng.gen_range(0.3..1.0);
            let px = from.x + dir_x * along + perp_x * (bow_off + tremor);
            let py = from.y + dir_y * along + perp_y * (bow_off - tremor);
            out.push(MotionSample { x: px.round() as i32, y: py.round() as i32, t_us: grid_100us(t_us) });
            if k + 1 < n {
                t_us += base_dt_us * self.rng.gen_range(0.85..1.15);
            }
        }
        if let Some(last) = out.last_mut() {
            last.x = to.x.round() as i32;
            last.y = to.y.round() as i32;
        }
        out
    }
}

// ================= Keystroke generator (P3) =================

/// A single keyboard event: down/up with the key + physical code, at a relative µs timestamp.
#[derive(Debug, Clone, PartialEq)]
pub enum KeyEventKind {
    KeyDown,
    KeyUp,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KeyEvent {
    pub kind: KeyEventKind,
    pub key: String,
    pub code: String,
    pub t_us: f64,
}

fn kind_rank(k: &KeyEventKind) -> u8 {
    match k {
        KeyEventKind::KeyDown => 0,
        KeyEventKind::KeyUp => 1,
    }
}

/// A standard-normal deviate scaled to (mean, sd) via Box–Muller.
fn normal(rng: &mut StdRng, mean: f64, sd: f64) -> f64 {
    let u1: f64 = rng.gen::<f64>().max(1e-12);
    let u2: f64 = rng.gen::<f64>();
    let z = (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos();
    mean + sd * z
}

/// Key-hold (dwell) time in µs: ~N(115 ms, 24), floored at 50 ms (Aalto 136M-keystroke).
fn sample_dwell_us(rng: &mut StdRng) -> f64 {
    normal(rng, 115.0, 24.0).max(50.0) * 1000.0
}

/// Inter-key interval (keydown → next keydown) in µs: log-normal (median ~205 ms, σ≈0.55 → mean ~238 ms,
/// skew ~2), floored at 60 ms, with a **per-finger** digraph adjustment (the Aalto structure): the SAME
/// finger typing two keys is slowest (one finger can't reposition fast), same-hand-different-finger is
/// medium, and hand-alternation is fastest.
fn sample_iki_us(rng: &mut StdRng, prev: Option<char>, cur: Option<char>) -> f64 {
    let z = normal(rng, 0.0, 1.0);
    let mut ms = (5.32_f64 + 0.55 * z).exp();
    if let (Some(fa), Some(fb)) = (prev.and_then(finger), cur.and_then(finger)) {
        let same_hand = (fa < 4) == (fb < 4);
        if fa == fb {
            ms += 50.0; // same finger — slowest
        } else if same_hand {
            ms += 15.0; // same hand, different finger
        } else {
            ms -= 35.0; // hand alternation — fastest
        }
    }
    ms.max(60.0) * 1000.0
}

/// QWERTY finger assignment (0-3 = left pinky→index, 4-7 = right index→pinky). None = non-letter/digit.
/// Drives the per-finger digraph timing structure.
fn finger(ch: char) -> Option<u8> {
    match ch.to_ascii_lowercase() {
        'q' | 'a' | 'z' | '1' => Some(0),                                     // L pinky
        'w' | 's' | 'x' | '2' => Some(1),                                     // L ring
        'e' | 'd' | 'c' | '3' => Some(2),                                     // L middle
        'r' | 'f' | 'v' | 't' | 'g' | 'b' | '4' | '5' => Some(3),             // L index
        'y' | 'h' | 'n' | 'u' | 'j' | 'm' | '6' | '7' => Some(4),             // R index
        'i' | 'k' | '8' => Some(5),                                           // R middle
        'o' | 'l' | '9' => Some(6),                                           // R ring
        'p' | '0' => Some(7),                                                 // R pinky
        _ => None,
    }
}

/// Map a character to (`KeyboardEvent.key`, `KeyboardEvent.code`) on the US layout -- the physical key
/// a person presses for it. Punctuation once had NO code (`'@'` answered `code: ""`, Chrome
/// `"Digit2"`, recorded with real X11 keys), and a newline was a key named `"\n"` where the key is Enter.
fn key_and_code(ch: char) -> (String, String) {
    const SHIFTED_DIGITS: &str = ")!@#$%^&*(";
    let code = match ch {
        ' ' => "Space".to_string(),
        '\n' | '\r' => return ("Enter".into(), "Enter".into()),
        '\t' => return ("Tab".into(), "Tab".into()),
        c if c.is_ascii_alphabetic() => format!("Key{}", c.to_ascii_uppercase()),
        c if c.is_ascii_digit() => format!("Digit{c}"),
        c if SHIFTED_DIGITS.contains(c) => format!("Digit{}", SHIFTED_DIGITS.find(c).unwrap_or(0)),
        '-' | '_' => "Minus".into(),
        '=' | '+' => "Equal".into(),
        '[' | '{' => "BracketLeft".into(),
        ']' | '}' => "BracketRight".into(),
        '\\' | '|' => "Backslash".into(),
        ';' | ':' => "Semicolon".into(),
        '\'' | '"' => "Quote".into(),
        ',' | '<' => "Comma".into(),
        '.' | '>' => "Period".into(),
        '/' | '?' => "Slash".into(),
        '`' | '~' => "Backquote".into(),
        _ => String::new(),
    };
    (ch.to_string(), code)
}

/// A character a US keyboard types only with Shift held: the capitals and the upper half of every
/// symbol key. The stream once typed them with no Shift at all, so every `'A'` and `'@'` arrived
/// with `shiftKey: false` -- a keystroke no keyboard produces.
fn needs_shift(ch: char) -> bool {
    ch.is_ascii_uppercase() || "~!@#$%^&*()_+{}|:\"<>?".contains(ch)
}

/// A plausible adjacent-row typo for `ch` — drives the ~6% error+correction rate.
fn typo_char(ch: char, rng: &mut StdRng) -> char {
    let row = "qwertyuiopasdfghjklzxcvbnm";
    if let Some(i) = row.find(ch.to_ascii_lowercase()) {
        let b = row.as_bytes();
        let j = if rng.gen::<bool>() && i + 1 < b.len() {
            i + 1
        } else if i > 0 {
            i - 1
        } else {
            i + 1
        };
        return b[j] as char;
    }
    ch
}

/// The keystroke generator (P3). Human dwell / IKI distributions, digraph structure, rollover (next key
/// pressed before the previous releases → negative flight, surfaced by time-ordering the stream), and
/// ~6% backspace-corrected typos. Its own PRNG stream, decorrelated from the mouse source.
pub struct KeystrokeSource {
    rng: StdRng,
}

impl KeystrokeSource {
    pub fn new(motor: MotorProfile) -> Self {
        Self { rng: StdRng::seed_from_u64(motor.seed ^ 0x6b65_7973_7472_6f6b) }
    }

    /// Generate the trusted-key event stream for `text` (time-ordered; keydown precedes keyup at equal t).
    pub fn type_text(&mut self, text: &str) -> Vec<KeyEvent> {
        let chars: Vec<char> = text.chars().collect();
        let mut evs: Vec<KeyEvent> = Vec::new();
        let mut t_down = 0.0_f64;
        let mut prev: Option<char> = None;
        for (i, &ch) in chars.iter().enumerate() {
            // ~6% of characters: a neighbour typo, then Backspace, then the correct key.
            if self.rng.gen::<f64>() < 0.06 {
                let wrong = typo_char(ch, &mut self.rng);
                let (wk, wc) = key_and_code(wrong);
                let d = sample_dwell_us(&mut self.rng);
                evs.push(KeyEvent { kind: KeyEventKind::KeyDown, key: wk.clone(), code: wc.clone(), t_us: t_down });
                evs.push(KeyEvent { kind: KeyEventKind::KeyUp, key: wk, code: wc, t_us: t_down + d });
                t_down += sample_iki_us(&mut self.rng, prev, Some(wrong));
                let bd = sample_dwell_us(&mut self.rng);
                evs.push(KeyEvent { kind: KeyEventKind::KeyDown, key: "Backspace".into(), code: "Backspace".into(), t_us: t_down });
                evs.push(KeyEvent { kind: KeyEventKind::KeyUp, key: "Backspace".into(), code: "Backspace".into(), t_us: t_down + bd });
                t_down += sample_iki_us(&mut self.rng, None, None);
                prev = None;
            }
            let (k, c) = key_and_code(ch);
            let d = sample_dwell_us(&mut self.rng);
            // A shifted character is Shift down a beat BEFORE the key and up a beat AFTER it.
            // The draws happen only for such a character, so an unshifted text's stream is unchanged.
            let shift = if needs_shift(ch) {
                let lead = normal(&mut self.rng, 90.0, 25.0).max(30.0) * 1000.0;
                let lag = normal(&mut self.rng, 60.0, 20.0).max(15.0) * 1000.0;
                if t_down < lead {
                    t_down = lead;
                }
                Some((t_down - lead, lag))
            } else {
                None
            };
            if let Some((at, _)) = shift {
                evs.push(KeyEvent { kind: KeyEventKind::KeyDown, key: "Shift".into(), code: "ShiftLeft".into(), t_us: at });
            }
            evs.push(KeyEvent { kind: KeyEventKind::KeyDown, key: k.clone(), code: c.clone(), t_us: t_down });
            evs.push(KeyEvent { kind: KeyEventKind::KeyUp, key: k, code: c, t_us: t_down + d });
            if let Some((_, lag)) = shift {
                evs.push(KeyEvent { kind: KeyEventKind::KeyUp, key: "Shift".into(), code: "ShiftLeft".into(), t_us: t_down + d + lag });
            }
            if i + 1 < chars.len() {
                t_down += sample_iki_us(&mut self.rng, prev, Some(ch));
            }
            prev = Some(ch);
        }
        // Time-order so rollover (a keydown before the previous keyup) shows in the stream; keydown before
        // keyup at an equal timestamp.
        evs.sort_by(|a, b| {
            a.t_us.partial_cmp(&b.t_us).unwrap().then_with(|| kind_rank(&a.kind).cmp(&kind_rank(&b.kind)))
        });
        evs
    }
}

// ================= Scroll / wheel generator (P4) =================

/// The wheel device class — governs the delta shape (persona-coherent: a mouse emits quantized notches;
/// a trackpad emits many fractional deltas with a momentum tail).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WheelKind {
    Mouse,
    Trackpad,
}

impl WheelKind {
    /// Pick the wheel class from the persona platform (macOS defaults to a trackpad).
    pub fn from_platform(platform: &str) -> Self {
        if platform.contains("Mac") {
            WheelKind::Trackpad
        } else {
            WheelKind::Mouse
        }
    }
}

/// A single wheel event: pixel deltas (`deltaMode` 0) at a relative µs timestamp.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WheelEvent {
    pub delta_x: f64,
    pub delta_y: f64,
    pub t_us: f64,
}

/// Windows wheel notch in pixel mode (deltaY quantizes to multiples of this).
const MOUSE_NOTCH_PX: f64 = 120.0;

/// The scroll generator (P4). One `scroll` call = one human gesture (a burst), not a fixed cadence of
/// identical steps. Reading pauses BETWEEN gestures are the ambient driver's job (P6).
pub struct ScrollSource {
    rng: StdRng,
    kind: WheelKind,
}

impl ScrollSource {
    pub fn new(motor: MotorProfile, kind: WheelKind) -> Self {
        Self { rng: StdRng::seed_from_u64(motor.seed ^ 0x7363_726f_6c6c), kind }
    }

    pub fn kind(&self) -> WheelKind {
        self.kind
    }

    /// Scroll ~`total_dy` pixels (sign = direction) as one gesture; returns the wheel event stream.
    pub fn scroll(&mut self, total_dy: f64) -> Vec<WheelEvent> {
        match self.kind {
            WheelKind::Mouse => self.scroll_mouse(total_dy),
            WheelKind::Trackpad => self.scroll_trackpad(total_dy),
        }
    }

    /// Mouse wheel: a handful of quantized notches (deltaY = ±120·k), spaced by tens–hundreds of ms.
    fn scroll_mouse(&mut self, total_dy: f64) -> Vec<WheelEvent> {
        let sign = if total_dy < 0.0 { -1.0 } else { 1.0 };
        let n = ((total_dy.abs() / MOUSE_NOTCH_PX).round() as usize).max(1);
        let mut t_us = 0.0;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            out.push(WheelEvent { delta_x: 0.0, delta_y: sign * MOUSE_NOTCH_PX, t_us });
            if i + 1 < n {
                t_us += self.rng.gen_range(60_000.0..160_000.0); // 60–160 ms between notches (jittered)
            }
        }
        out
    }

    /// Trackpad: many small fractional deltas ramping to a peak, then an exponential-decay MOMENTUM tail
    /// (indistinguishable from user-initiated on macOS), all at ~16.7 ms (60 Hz) cadence with jitter.
    fn scroll_trackpad(&mut self, total_dy: f64) -> Vec<WheelEvent> {
        let sign = if total_dy < 0.0 { -1.0 } else { 1.0 };
        let mag = total_dy.abs().max(1.0);
        let active_n = self.rng.gen_range(8..16usize);
        // Peak per-event delta so the active + momentum phases roughly sum to `mag`.
        let peak = (mag / (active_n as f64 * 1.8)).clamp(1.0, 200.0);
        let mut out = Vec::new();
        let mut t_us = 0.0;
        let push = |out: &mut Vec<WheelEvent>, d: f64, t: f64| {
            out.push(WheelEvent { delta_x: 0.0, delta_y: sign * d, t_us: t });
        };
        // Active phase: smooth ramp 0→peak→(finger lift near peak).
        for i in 0..active_n {
            let frac = (i as f64 + 1.0) / active_n as f64;
            let ramp = (frac * std::f64::consts::PI).sin().abs().max(0.15); // bell-ish, never 0
            let jitter = self.rng.gen_range(0.85..1.15);
            push(&mut out, (peak * ramp * jitter).max(0.5), t_us);
            t_us += 16_700.0 * self.rng.gen_range(0.9..1.1);
        }
        // Momentum phase: exponential decay from ~peak until below the min wheel delta (~0.5 px).
        let decay = self.rng.gen_range(0.82..0.9);
        let mut v = peak * self.rng.gen_range(0.7..1.0);
        let mut guard = 0;
        while v > 0.5 && guard < 60 {
            push(&mut out, v, t_us);
            v *= decay;
            t_us += 16_700.0 * self.rng.gen_range(0.9..1.1);
            guard += 1;
        }
        out
    }
}

// ================= Touch generator (mobile persona) =================

/// The phase of a touch sample — selects the event type (touchstart / touchmove / touchend).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TouchPhase {
    Start,
    Move,
    End,
}

/// A single touch sample: position + the ELLIPTICAL contact (radiusX ≠ radiusY) + force (0..1) at a
/// relative µs timestamp. `force` traces a rise→plateau→release curve; a constant/zero force or
/// radiusX==radiusY is a desktop-emulation tell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TouchSample {
    pub x: i32,
    pub y: i32,
    pub t_us: f64,
    pub force: f64,
    pub radius_x: f64,
    pub radius_y: f64,
    pub phase: TouchPhase,
}

/// The touch generator (mobile persona): taps and swipes with a human force curve, an elliptical contact
/// (radiusX ≠ radiusY, 0.25 px quantized like a real controller), and a tap dwell / swipe fling. Its own
/// PRNG stream.
pub struct TouchSource {
    rng: StdRng,
}

impl TouchSource {
    pub fn new(motor: MotorProfile) -> Self {
        Self { rng: StdRng::seed_from_u64(motor.seed ^ 0x746f_7563_68) }
    }

    /// An elliptical finger contact: radiusX ≠ radiusY (taller than wide), quantized to 0.25 px.
    fn contact(&mut self) -> (f64, f64) {
        let q = |v: f64| (v * 4.0).round() / 4.0;
        let rx = self.rng.gen_range(8.0..16.0);
        let ry = rx * self.rng.gen_range(1.1..1.5);
        (q(rx), q(ry))
    }

    /// A tap: touchstart (finger down, force at its plateau) → touchend (release, force 0), ~N(90 ms, 25)
    /// dwell. One finger; identifier 0.
    pub fn tap(&mut self, x: f64, y: f64) -> Vec<TouchSample> {
        let (rx, ry) = self.contact();
        let dwell_us = normal(&mut self.rng, 90.0, 25.0).max(50.0) * 1000.0;
        let force = self.rng.gen_range(0.35..0.65);
        let (xi, yi) = (x.round() as i32, y.round() as i32);
        vec![
            TouchSample { x: xi, y: yi, t_us: 0.0, force, radius_x: rx, radius_y: ry, phase: TouchPhase::Start },
            TouchSample { x: xi, y: yi, t_us: dwell_us, force: 0.0, radius_x: rx, radius_y: ry, phase: TouchPhase::End },
        ]
    }

    /// A swipe: touchstart → touchmove* along the path (force at a plateau) → touchend, with a fling (the
    /// last moves speed up before release). Path resampled to ~60 Hz over a Fitts-scaled duration.
    pub fn swipe(&mut self, from: Point, to: Point) -> Vec<TouchSample> {
        let (rx, ry) = self.contact();
        let dist = ((to.x - from.x).powi(2) + (to.y - from.y).powi(2)).sqrt();
        let dur_ms = (200.0 + dist * 0.6).min(1200.0); // swipes are quick
        let n = ((dur_ms * 60.0 / 1000.0).round() as usize).clamp(4, 120);
        let force = self.rng.gen_range(0.3..0.5);
        let mut out = Vec::with_capacity(n + 1);
        let mut t_us = 0.0;
        for i in 0..n {
            let frac = i as f64 / (n - 1) as f64;
            // ease-in fling: accelerate toward the end
            let ef = frac * frac;
            let px = from.x + (to.x - from.x) * ef;
            let py = from.y + (to.y - from.y) * ef;
            let phase = if i == 0 { TouchPhase::Start } else { TouchPhase::Move };
            out.push(TouchSample { x: px.round() as i32, y: py.round() as i32, t_us: grid_100us(t_us), force, radius_x: rx, radius_y: ry, phase });
            t_us += (dur_ms * 1000.0 / n as f64) * self.rng.gen_range(0.85..1.15);
        }
        out.push(TouchSample { x: to.x.round() as i32, y: to.y.round() as i32, t_us: grid_100us(t_us), force: 0.0, radius_x: rx, radius_y: ry, phase: TouchPhase::End });
        out
    }
}

/// The offline **behavioral oracle** — the honest adversary (a device-check script `es_*`, Castle rules, human
/// distributions). Compute the detector's own features on a generated stream and assert human range;
/// NEVER widen a band to fake a pass (STOP condition). Feature analysis only — no v8, no network.
pub mod oracle {
    use super::{KeyEvent, KeyEventKind, TouchSample, WheelEvent};
    use spec::MotionSample;

    pub fn mean(xs: &[f64]) -> f64 {
        if xs.is_empty() {
            return 0.0;
        }
        xs.iter().sum::<f64>() / xs.len() as f64
    }

    pub fn stddev(xs: &[f64]) -> f64 {
        if xs.len() < 2 {
            return 0.0;
        }
        let m = mean(xs);
        (xs.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (xs.len() - 1) as f64).sqrt()
    }

    /// Consecutive inter-sample Δt (µs).
    pub fn dt_us(s: &[MotionSample]) -> Vec<f64> {
        s.windows(2).map(|w| w[1].t_us - w[0].t_us).collect()
    }

    /// Tangential velocity per step (px per µs).
    pub fn velocities(s: &[MotionSample]) -> Vec<f64> {
        s.windows(2)
            .map(|w| {
                let d = (((w[1].x - w[0].x) as f64).powi(2) + ((w[1].y - w[0].y) as f64).powi(2)).sqrt();
                let dt = (w[1].t_us - w[0].t_us).max(1.0);
                d / dt
            })
            .collect()
    }

    /// Per-step acceleration magnitude (Δvelocity).
    pub fn accelerations(s: &[MotionSample]) -> Vec<f64> {
        let v = velocities(s);
        v.windows(2).map(|w| (w[1] - w[0]).abs()).collect()
    }

    /// Max consecutive step distance (px) — a teleport / discontinuity check.
    pub fn max_step_px(s: &[MotionSample]) -> f64 {
        s.windows(2)
            .map(|w| (((w[1].x - w[0].x) as f64).powi(2) + ((w[1].y - w[0].y) as f64).powi(2)).sqrt())
            .fold(0.0, f64::max)
    }

    /// a device-check script `es_sigmdn`: the stddev of `log(timeStamp)` across the pool — the timing-entropy feature.
    /// A degenerate (≈0) value means uniform/quantized timing → bot. Computed over samples with t>0.
    pub fn es_sigmdn(s: &[MotionSample]) -> f64 {
        let logs: Vec<f64> = s.iter().filter(|p| p.t_us > 0.0).map(|p| p.t_us.ln()).collect();
        stddev(&logs)
    }

    /// True iff the velocity profile is bell-ish: the middle third is faster than the outer sixths
    /// (humans accelerate then decelerate; a constant-velocity line fails this).
    pub fn velocity_is_bell(s: &[MotionSample]) -> bool {
        let v = velocities(s);
        if v.len() < 6 {
            return false;
        }
        let n = v.len();
        let mid = mean(&v[n / 3..2 * n / 3]);
        let mut ends = v[..n / 6].to_vec();
        ends.extend_from_slice(&v[5 * n / 6..]);
        mid > mean(&ends)
    }

    /// True iff the velocity peaks BEFORE the midpoint — the Sigma-lognormal asymmetry (humans accelerate
    /// faster than they decelerate). A symmetric (minimum-jerk) or constant profile fails this.
    pub fn velocity_is_asymmetric(s: &[MotionSample]) -> bool {
        let v = velocities(s);
        if v.len() < 8 {
            return false;
        }
        let peak_i = v
            .iter()
            .enumerate()
            .fold((0usize, 0.0f64), |(bi, bv), (i, &x)| if x > bv { (i, x) } else { (bi, bv) })
            .0;
        (peak_i as f64) < 0.6 * v.len() as f64
    }

    /// True iff acceleration is not degenerate — i.e. the stream is NOT a constant-velocity line
    /// (Castle: human acceleration "is almost never close to zero"; a straight bot path has accel≈0).
    pub fn accel_nondegenerate(s: &[MotionSample]) -> bool {
        stddev(&velocities(s)) > 1e-6 && accelerations(s).iter().cloned().fold(0.0, f64::max) > 1e-6
    }

    /// True iff Δt is jittered (not constant) — constant sampling is an LSTM-catchable tell.
    pub fn dt_is_jittered(s: &[MotionSample]) -> bool {
        stddev(&dt_us(s)) > 1.0
    }

    // ---- keystroke features ----

    /// Key-hold (dwell) times in µs: FIFO-match each KeyDown to the next KeyUp of the same key.
    pub fn dwell_times_us(evs: &[KeyEvent]) -> Vec<f64> {
        use std::collections::HashMap;
        let mut pending: HashMap<&str, Vec<f64>> = HashMap::new();
        let mut out = Vec::new();
        for e in evs {
            match e.kind {
                KeyEventKind::KeyDown => pending.entry(&e.key).or_default().push(e.t_us),
                KeyEventKind::KeyUp => {
                    if let Some(q) = pending.get_mut(e.key.as_str()) {
                        if !q.is_empty() {
                            let down = q.remove(0);
                            out.push(e.t_us - down);
                        }
                    }
                }
            }
        }
        out
    }

    /// Inter-key intervals (keydown → next keydown) in µs.
    pub fn iki_us(evs: &[KeyEvent]) -> Vec<f64> {
        let downs: Vec<f64> = evs.iter().filter(|e| e.kind == KeyEventKind::KeyDown).map(|e| e.t_us).collect();
        downs.windows(2).map(|w| w[1] - w[0]).collect()
    }

    /// The minimum inter-key interval (µs) — a superhuman-speed floor check (human ≥ ~60 ms).
    pub fn min_iki_us(evs: &[KeyEvent]) -> f64 {
        iki_us(evs).into_iter().fold(f64::INFINITY, f64::min)
    }

    /// Fraction of keydowns that occur while a previous key is still held (rollover / negative flight).
    /// Zero rollover is a strong bot tell; humans overlap 25–50% of keys.
    pub fn rollover_fraction(evs: &[KeyEvent]) -> f64 {
        let mut held = 0i32;
        let mut downs = 0u32;
        let mut roll = 0u32;
        for e in evs {
            match e.kind {
                KeyEventKind::KeyDown => {
                    downs += 1;
                    if held >= 1 {
                        roll += 1;
                    }
                    held += 1;
                }
                KeyEventKind::KeyUp => held = (held - 1).max(0),
            }
        }
        if downs == 0 {
            0.0
        } else {
            roll as f64 / downs as f64
        }
    }

    /// Fraction of keydowns that are Backspace (error-correction rate; humans ~6%).
    pub fn backspace_fraction(evs: &[KeyEvent]) -> f64 {
        let downs: Vec<&KeyEvent> = evs.iter().filter(|e| e.kind == KeyEventKind::KeyDown).collect();
        if downs.is_empty() {
            return 0.0;
        }
        downs.iter().filter(|e| e.key == "Backspace").count() as f64 / downs.len() as f64
    }

    // ---- scroll / wheel features ----

    /// Consecutive wheel inter-event Δt (µs).
    pub fn wheel_dt_us(evs: &[WheelEvent]) -> Vec<f64> {
        evs.windows(2).map(|w| w[1].t_us - w[0].t_us).collect()
    }

    /// True iff every deltaY is an integer multiple of `notch` (the mouse-wheel quantization signature).
    pub fn is_all_multiple_of(evs: &[WheelEvent], notch: f64) -> bool {
        !evs.is_empty()
            && evs.iter().all(|e| {
                let r = e.delta_y.abs() / notch;
                (r - r.round()).abs() < 1e-6
            })
    }

    /// True iff any deltaY has a fractional part (the trackpad signature — never clean notches).
    pub fn any_fractional(evs: &[WheelEvent]) -> bool {
        evs.iter().any(|e| e.delta_y.abs().fract() > 1e-6)
    }

    /// True iff the tail decays monotonically (an exponential momentum tail after finger-lift). Absence
    /// on a trackpad profile is a bot tell.
    pub fn has_momentum_tail(evs: &[WheelEvent]) -> bool {
        let mags: Vec<f64> = evs.iter().map(|e| e.delta_y.abs()).collect();
        if mags.len() < 4 {
            return false;
        }
        let tail = &mags[mags.len().saturating_sub(5)..];
        tail.windows(2).all(|w| w[1] <= w[0] + 1e-9) && tail.first().unwrap() > tail.last().unwrap()
    }

    // ---- touch features ----

    /// True iff every contact is elliptical (radiusX ≠ radiusY) — a real finger; radiusX==radiusY (or a
    /// constant 0.5/1.0) is the desktop-emulation tell.
    pub fn touch_radius_is_elliptical(s: &[TouchSample]) -> bool {
        !s.is_empty() && s.iter().all(|t| (t.radius_x - t.radius_y).abs() > 0.1)
    }

    /// True iff the touch stream carries a non-zero force at some point (a real pressure sensor), not a
    /// flat zero.
    pub fn touch_has_force(s: &[TouchSample]) -> bool {
        s.iter().any(|t| t.force > 0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spec::{MotionSample, MotorProfile, Point, TrajectorySource};

    fn src(seed: u64) -> NeuromotorSource {
        NeuromotorSource::new(MotorProfile { seed, ..MotorProfile::default() })
    }

    /// Fitts monotonicity: farther / smaller targets take longer; a tiny move hits the floor.
    #[test]
    fn fitts_scales_with_index_of_difficulty() {
        assert!(fitts_mt_ms(500.0, 40.0) > fitts_mt_ms(100.0, 40.0));
        assert!(fitts_mt_ms(500.0, 10.0) > fitts_mt_ms(500.0, 200.0));
        assert!(fitts_mt_ms(1.0, 500.0) >= 40.0);
    }

    /// A normal move is a poll-rate-dense, connected, on-target human stream — and passes the oracle.
    #[test]
    fn p1_neuromotor_move_passes_the_behavioral_oracle() {
        let mut s = src(42);
        let from = Point::new(20.0, 30.0);
        let to = Point::new(520.0, 380.0);
        let m = s.plan_move(from, to, 40.0);

        // Count is poll-rate × Fitts-duration (dozens–hundreds for a real move), not a teleporting handful.
        let dist = ((to.x - from.x).powi(2) + (to.y - from.y).powi(2)).sqrt();
        let expect = (fitts_mt_ms(dist, 40.0) * 125.0 / 1000.0).round() as usize;
        assert!(m.len() >= 40, "too few samples ({}) — a teleport tell", m.len());
        assert!((m.len() as i64 - expect as i64).abs() <= 2, "count {} != poll×MT {}", m.len(), expect);

        // Lands on target; connected (no teleport); monotonically-increasing timestamps.
        assert_eq!((m.last().unwrap().x, m.last().unwrap().y), (520, 380));
        assert!(oracle::max_step_px(&m) < 60.0, "teleport: max step {}", oracle::max_step_px(&m));
        assert!(m.windows(2).all(|w| w[1].t_us >= w[0].t_us), "timestamps must be monotonic");

        // The detector's own features land in human range.
        assert!(oracle::es_sigmdn(&m) > 0.05, "es_sigmdn degenerate: {}", oracle::es_sigmdn(&m));
        assert!(oracle::dt_is_jittered(&m), "Δt must be jittered, not constant");
        assert!(oracle::accel_nondegenerate(&m), "accel≈0 everywhere = constant-velocity bot line");
        assert!(oracle::velocity_is_bell(&m), "velocity must accelerate then decelerate (bell)");
    }

    /// Anti-replay by construction: successive moves on one source (PRNG advances) never repeat, even
    /// for the identical from/to/target.
    #[test]
    fn p1_successive_moves_never_repeat() {
        let mut s = src(7);
        let a = s.plan_move(Point::new(0.0, 0.0), Point::new(400.0, 300.0), 40.0);
        let b = s.plan_move(Point::new(0.0, 0.0), Point::new(400.0, 300.0), 40.0);
        assert_ne!(a, b, "identical trajectories would be replay-detectable (ReMouse)");
    }

    /// Determinism for testing: same seed → same stream (reproducible sessions).
    #[test]
    fn p1_same_seed_is_reproducible() {
        let a = src(99).plan_move(Point::new(0.0, 0.0), Point::new(300.0, 200.0), 40.0);
        let b = src(99).plan_move(Point::new(0.0, 0.0), Point::new(300.0, 200.0), 40.0);
        assert_eq!(a, b, "same seed must reproduce the same trajectory");
    }

    /// A zero-length move degrades to a single on-target sample (no panic, no NaN).
    #[test]
    fn p1_degenerate_move_is_safe() {
        let m = src(1).plan_move(Point::new(100.0, 100.0), Point::new(100.0, 100.0), 40.0);
        assert_eq!(m.len(), 1);
        assert_eq!((m[0].x, m[0].y), (100, 100));
    }

    /// A synthetic constant-velocity straight line must FAIL the oracle (proving the oracle discriminates,
    /// not just rubber-stamps) — the classic linear-bot tell Castle catches.
    #[test]
    fn p1_oracle_rejects_a_constant_velocity_line() {
        let line: Vec<MotionSample> =
            (0..100).map(|i| MotionSample { x: i * 5, y: i * 3, t_us: i as f64 * 8000.0 }).collect();
        assert!(!oracle::accel_nondegenerate(&line), "constant-velocity line must be flagged");
        assert!(!oracle::dt_is_jittered(&line), "constant Δt must be flagged");
    }

    fn sigma(seed: u64) -> SigmaLognormalSource {
        SigmaLognormalSource::new(MotorProfile { seed, ..MotorProfile::default() })
    }

    /// Sigma-lognormal: a move is poll-rate dense, on-target, connected, passes the oracle AND has the
    /// asymmetric (early-peak) velocity that WindMouse/min-jerk don't guarantee.
    #[test]
    fn sigma_lognormal_move_passes_oracle_and_is_asymmetric() {
        let mut s = sigma(42);
        let m = s.plan_move(Point::new(20.0, 30.0), Point::new(520.0, 380.0), 40.0);
        assert!(m.len() >= 40, "poll-rate dense, got {}", m.len());
        assert_eq!((m.last().unwrap().x, m.last().unwrap().y), (520, 380), "lands on target");
        assert!(oracle::max_step_px(&m) < 60.0, "no teleport: {}", oracle::max_step_px(&m));
        assert!(m.windows(2).all(|w| w[1].t_us >= w[0].t_us), "monotonic timestamps");
        assert!(oracle::es_sigmdn(&m) > 0.05, "es_sigmdn degenerate: {}", oracle::es_sigmdn(&m));
        assert!(oracle::dt_is_jittered(&m), "Δt must be jittered");
        assert!(oracle::accel_nondegenerate(&m), "accel must vary");
        assert!(oracle::velocity_is_bell(&m), "velocity must be a bell");
        assert!(oracle::velocity_is_asymmetric(&m), "Sigma-lognormal velocity must peak early (asymmetric)");
    }

    /// Sigma-lognormal is reproducible per seed and never repeats within a session (anti-replay).
    #[test]
    fn sigma_lognormal_anti_replay_and_reproducible() {
        let a = sigma(9).plan_move(Point::new(0.0, 0.0), Point::new(400.0, 300.0), 40.0);
        let b = sigma(9).plan_move(Point::new(0.0, 0.0), Point::new(400.0, 300.0), 40.0);
        assert_eq!(a, b, "same seed must reproduce");
        let mut s = sigma(7);
        let c = s.plan_move(Point::new(0.0, 0.0), Point::new(400.0, 300.0), 40.0);
        let d = s.plan_move(Point::new(0.0, 0.0), Point::new(400.0, 300.0), 40.0);
        assert_ne!(c, d, "successive moves must not repeat");
    }

    fn replay(seed: u64) -> RecordedReplaySource {
        RecordedReplaySource::new(MotorProfile { seed, ..MotorProfile::default() })
    }

    /// Recorded-replay (real SapiMouse human velocity dynamics warped to the move): poll-rate dense,
    /// on-target, passes the oracle, reproducible per seed, anti-replay across moves.
    #[test]
    fn recorded_replay_move_passes_oracle_and_anti_replay() {
        let mut s = replay(42);
        let m = s.plan_move(Point::new(20.0, 30.0), Point::new(520.0, 380.0), 40.0);
        assert!(m.len() >= 40, "poll-rate dense, got {}", m.len());
        assert_eq!((m.last().unwrap().x, m.last().unwrap().y), (520, 380), "lands on target");
        assert!(oracle::max_step_px(&m) < 60.0, "no teleport: {}", oracle::max_step_px(&m));
        assert!(m.windows(2).all(|w| w[1].t_us >= w[0].t_us), "monotonic timestamps");
        assert!(oracle::es_sigmdn(&m) > 0.05, "es_sigmdn degenerate: {}", oracle::es_sigmdn(&m));
        assert!(oracle::dt_is_jittered(&m), "Δt must be jittered");
        assert!(oracle::accel_nondegenerate(&m), "real human acceleration varies");
        assert_eq!(
            replay(9).plan_move(Point::new(0.0, 0.0), Point::new(300.0, 200.0), 40.0),
            replay(9).plan_move(Point::new(0.0, 0.0), Point::new(300.0, 200.0), 40.0),
            "same seed reproduces",
        );
        let mut z = replay(3);
        let a = z.plan_move(Point::new(0.0, 0.0), Point::new(300.0, 200.0), 40.0);
        let b = z.plan_move(Point::new(0.0, 0.0), Point::new(300.0, 200.0), 40.0);
        assert_ne!(a, b, "successive moves must not repeat (anti-replay)");
    }

    fn kstroke(seed: u64) -> KeystrokeSource {
        KeystrokeSource::new(MotorProfile { seed, ..MotorProfile::default() })
    }

    /// P3: generated typing lands in human dwell/IKI/rollover/backspace ranges (Aalto 136M) — and no
    /// superhuman sub-60ms keystrokes.
    #[test]
    fn p3_keystroke_distributions_are_human() {
        let mut k = kstroke(5);
        let evs = k.type_text(
            "the quick brown fox jumps over the lazy dog while typing some more realistic text here today",
        );
        let dwell = oracle::dwell_times_us(&evs);
        let mean_dwell = oracle::mean(&dwell) / 1000.0;
        assert!((80.0..160.0).contains(&mean_dwell), "dwell mean {mean_dwell}ms out of human range");

        let iki = oracle::iki_us(&evs);
        let mean_iki = oracle::mean(&iki) / 1000.0;
        assert!((120.0..400.0).contains(&mean_iki), "IKI mean {mean_iki}ms out of human range");
        assert!(oracle::stddev(&iki) > 1000.0, "IKI must vary, not be constant");
        assert!(oracle::min_iki_us(&evs) >= 55_000.0, "sub-60ms IKI is superhuman");

        let ro = oracle::rollover_fraction(&evs);
        assert!(ro > 0.05 && ro < 0.70, "rollover {ro} implausible (zero rollover is a bot tell)");

        let bs = oracle::backspace_fraction(&evs);
        assert!(bs > 0.0 && bs < 0.20, "backspace rate {bs} (humans correct errors ~6%)");
    }

    /// P3: typing is reproducible per seed and never repeats within a session (anti-replay).
    #[test]
    fn p3_keystroke_anti_replay_and_reproducible() {
        let a = kstroke(3).type_text("hello world");
        let b = kstroke(3).type_text("hello world");
        assert_eq!(a, b, "same seed must reproduce");
        let mut k = kstroke(9);
        let c = k.type_text("hello world");
        let d = k.type_text("hello world");
        assert_ne!(c, d, "successive typing must not repeat (anti-replay)");
    }

    fn scroller(seed: u64, kind: WheelKind) -> ScrollSource {
        ScrollSource::new(MotorProfile { seed, ..MotorProfile::default() }, kind)
    }

    /// P4: a mouse wheel emits a few quantized 120px notches with jittered timing.
    #[test]
    fn p4_mouse_wheel_is_quantized_notches() {
        let evs = scroller(11, WheelKind::Mouse).scroll(600.0);
        assert!((3..=8).contains(&evs.len()), "mouse: a few notches, got {}", evs.len());
        assert!(oracle::is_all_multiple_of(&evs, 120.0), "mouse deltaY must be 120-multiples");
        assert!(evs.iter().all(|e| e.delta_y > 0.0), "downward scroll is positive deltaY");
        assert!(oracle::stddev(&oracle::wheel_dt_us(&evs)) > 1000.0, "notch timing must jitter");
    }

    /// P4: a trackpad emits many fractional deltas with an exponential momentum tail — never clean notches.
    #[test]
    fn p4_trackpad_has_fractional_deltas_and_momentum() {
        let evs = scroller(12, WheelKind::Trackpad).scroll(600.0);
        assert!(evs.len() >= 12, "trackpad: many events, got {}", evs.len());
        assert!(oracle::any_fractional(&evs), "trackpad deltas must be fractional");
        assert!(oracle::has_momentum_tail(&evs), "trackpad must have an exponential momentum tail");
        assert!(!oracle::is_all_multiple_of(&evs, 120.0), "trackpad must NOT be clean 120-notches");
    }

    /// P4: successive scrolls never repeat (anti-replay).
    #[test]
    fn p4_scroll_anti_replay() {
        let mut s = scroller(4, WheelKind::Trackpad);
        let a = s.scroll(500.0);
        let b = s.scroll(500.0);
        assert_ne!(a, b, "successive scrolls must not repeat");
    }

    /// P5: synthetic element coordinates are stable per (key, seed), in-bounds, and vary by key/seed.
    #[test]
    fn p5_synth_coord_is_stable_and_in_bounds() {
        let a = synth_coord("#submit", 42, 1280, 800);
        assert_eq!(a, synth_coord("#submit", 42, 1280, 800), "must be stable per (key, seed)");
        assert_ne!(a, synth_coord("#cancel", 42, 1280, 800), "different elements differ");
        assert_ne!(a, synth_coord("#submit", 43, 1280, 800), "different sessions differ");
        assert!((16..=1264).contains(&a.0) && (16..=784).contains(&a.1), "in-bounds: {a:?}");
    }

    /// P5: the pre-click press-hold is a plausible human duration (≥50 ms).
    #[test]
    fn p5_press_dwell_is_human() {
        let mut s = src(1);
        for _ in 0..50 {
            let ms = s.press_dwell_us() / 1000.0;
            assert!((50.0..250.0).contains(&ms), "press dwell {ms}ms out of range");
        }
    }

    fn touch(seed: u64) -> TouchSource {
        TouchSource::new(MotorProfile { seed, ..MotorProfile::default() })
    }

    /// Touch tap: start→end with a force curve (down then release) and an elliptical contact.
    #[test]
    fn touch_tap_has_force_and_elliptical_contact() {
        let mut t = touch(3);
        let tap = t.tap(100.0, 200.0);
        assert_eq!(tap.len(), 2, "tap = touchstart + touchend");
        assert_eq!(tap[0].phase, TouchPhase::Start);
        assert_eq!(tap[1].phase, TouchPhase::End);
        assert!(tap[0].force > 0.0, "touchstart carries force (finger down)");
        assert_eq!(tap[1].force, 0.0, "touchend releases (force 0)");
        assert!(oracle::touch_radius_is_elliptical(&tap), "radiusX != radiusY (a real finger)");
        assert!(oracle::touch_has_force(&tap));
        assert!(tap[1].t_us >= 50_000.0, "tap dwell >= 50ms");
    }

    /// Touch swipe: many samples, start→end, lands on target, elliptical contact, anti-replay.
    #[test]
    fn touch_swipe_moves_and_lands() {
        let mut t = touch(5);
        let sw = t.swipe(Point::new(50.0, 600.0), Point::new(300.0, 200.0));
        assert!(sw.len() >= 5, "swipe has many samples, got {}", sw.len());
        assert_eq!(sw.first().unwrap().phase, TouchPhase::Start);
        assert_eq!(sw.last().unwrap().phase, TouchPhase::End);
        assert_eq!((sw.last().unwrap().x, sw.last().unwrap().y), (300, 200), "ends at the target");
        assert!(oracle::touch_radius_is_elliptical(&sw));
        let b = touch(5).swipe(Point::new(50.0, 600.0), Point::new(300.0, 200.0));
        assert_eq!(sw, b, "reproducible per seed");
    }
}
